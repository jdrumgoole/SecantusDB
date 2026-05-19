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


async def test_page_header_shows_target_server(server, http: AsyncClient) -> None:
    """Every page renders the target URI badge (sanitized)."""
    r = await http.get("/", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "server-badge" in r.text
    # Badge content is the URL the fixture actually started on (ephemeral
    # port chosen by the kernel). Always loopback in tests.
    assert "127.0.0.1" in r.text


# ---- /server — live target swap --------------------------------------------


async def test_connection_page_shows_current(http: AsyncClient) -> None:
    r = await http.get("/server", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Currently connected to" in r.text
    assert "Switch to a new target" in r.text


async def test_connection_switch_rebinds_facade(server, tmp_path) -> None:
    """Switching the URI through the page actually rebinds app.state.mongo."""
    from pymongo import MongoClient

    # Stand up a SECOND server so we have a valid alternate target.
    other_path = tmp_path / "other"
    other_path.mkdir()
    with SecantusDBServer(port=0, storage_path=str(other_path)) as srv2:
        # Seed each server with a unique collection so we can prove the
        # facade actually pivoted afterward.
        mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            mc["mark"]["a"].insert_one({"_id": 1, "where": "first"})
        finally:
            mc.close()
        mc = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            mc["mark"]["b"].insert_one({"_id": 1, "where": "second"})
        finally:
            mc.close()

        app = create_app(
            mongo_uri=server.uri,
            token="testtoken",
            history_path=tmp_path / "hist.db",
        )
        try:
            async with AsyncClient(
                transport=ASGITransport(app=app),
                base_url="http://testserver",
            ) as c:
                r = await c.post(
                    "/server/switch",
                    data={"uri": srv2.uri},
                    headers={HEADER_NAME: "testtoken"},
                )
                assert r.status_code == 200
                assert "Switched to" in r.text
                assert app.state.mongo_uri == srv2.uri
                # The new facade can see the second server's collection.
                names = [c["name"] for c in app.state.mongo.list_collections_with_stats("mark")]
                assert names == ["b"]
        finally:
            app.state.mongo.close()


async def test_connection_switch_rejects_unreachable(server, http: AsyncClient) -> None:
    r = await http.post(
        "/server/switch",
        data={"uri": "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=200"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "could not reach target" in r.text
    # Error must be cleaned — no pymongo topology dump / configured-timeouts
    # noise in the rendered page.
    assert "Topology Description" not in r.text
    assert "configured timeouts" not in r.text


async def test_connection_switch_rejects_blank_uri(http: AsyncClient) -> None:
    r = await http.post(
        "/server/switch",
        data={"uri": "   "},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "URI is required" in r.text


async def test_connection_recent_lists_after_switch(server, tmp_path) -> None:
    """Saved-target list reflects past switches; current is flagged."""
    other_path = tmp_path / "other"
    other_path.mkdir()
    with SecantusDBServer(port=0, storage_path=str(other_path)) as srv2:
        app = create_app(
            mongo_uri=server.uri,
            token="testtoken",
            history_path=tmp_path / "hist.db",
        )
        try:
            async with AsyncClient(
                transport=ASGITransport(app=app),
                base_url="http://testserver",
            ) as c:
                # Switch once so the new URI lands in the targets store.
                await c.post(
                    "/server/switch",
                    data={"uri": srv2.uri},
                    headers={HEADER_NAME: "testtoken"},
                )
                r = await c.get("/server", headers={HEADER_NAME: "testtoken"})
                assert r.status_code == 200
                # The newly-current URI shows up flagged "current".
                assert "current" in r.text
                # And the URI itself is in the page (sanitised form).
                assert srv2.uri.rstrip("/") in r.text
        finally:
            app.state.mongo.close()


async def test_connection_forget_removes_saved_uri(server, tmp_path) -> None:
    other_path = tmp_path / "other"
    other_path.mkdir()
    with SecantusDBServer(port=0, storage_path=str(other_path)):
        app = create_app(
            mongo_uri=server.uri,
            token="testtoken",
            history_path=tmp_path / "hist.db",
        )
        try:
            # Pre-record an unrelated URI in the store directly.
            saved = "mongodb://stale:1234/"
            app.state.targets.record(saved)
            async with AsyncClient(
                transport=ASGITransport(app=app),
                base_url="http://testserver",
            ) as c:
                r = await c.post(
                    "/server/forget",
                    data={"uri": saved},
                    headers={HEADER_NAME: "testtoken"},
                )
                assert r.status_code == 200
                assert "Forgot" in r.text
                assert all(e.uri != saved for e in app.state.targets.recent())
        finally:
            app.state.mongo.close()


async def test_connection_cannot_forget_current(http: AsyncClient, app) -> None:
    r = await http.post(
        "/server/forget",
        data={"uri": app.state.mongo_uri},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "currently connected" in r.text


async def test_page_header_strips_password(server, tmp_path) -> None:
    """Password in the URI doesn't leak into the rendered badge."""
    app = create_app(
        mongo_uri="mongodb://alice:s3cret@127.0.0.1:1/?authSource=admin",
        token="testtoken",
        history_path=tmp_path / "h.db",
    )
    try:
        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://testserver",
        ) as c:
            r = await c.get("/", headers={HEADER_NAME: "testtoken"})
            assert r.status_code == 200
            assert "alice@127.0.0.1" in r.text
            assert "s3cret" not in r.text
            assert "authSource" not in r.text
    finally:
        app.state.mongo.close()


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


async def test_collection_viewer_renders_first_page(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["viewer_db"]["c"].insert_many([{"_id": i, "name": f"row-{i}"} for i in range(7)])
    finally:
        mc.close()

    r = await http.get("/db/viewer_db/c?page_size=5", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # First five docs visible.
    assert "row-0" in r.text
    assert "row-4" in r.text
    # Sixth not on this page.
    assert "row-5" not in r.text
    # Next-page link present.
    assert "Next page" in r.text


async def test_collection_viewer_paginates_to_completion(server, http: AsyncClient) -> None:
    from urllib.parse import parse_qs, urlparse

    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["page_db"]["c"].insert_many([{"_id": i} for i in range(12)])
    finally:
        mc.close()

    url = "/db/page_db/c?page_size=5"
    for _ in range(10):  # safety bound
        r = await http.get(url, headers={HEADER_NAME: "testtoken"})
        assert r.status_code == 200
        # Pull the next-page href if present. The pager may also include
        # a "First page" link on non-first pages, so locate the next-page
        # anchor specifically by its href carrying ``cursor=``.
        text = r.text
        if "Next page" not in text:
            break
        # Extract the next-page anchor — the only one with a cursor param.
        marker = 'href="/db/page_db/c?'
        start = 0
        href = ""
        while True:
            start = text.find(marker, start)
            if start == -1:
                break
            end = text.find('"', start + 6)
            candidate = text[start + 6 : end].replace("&amp;", "&")
            if "cursor=" in candidate:
                href = candidate
                break
            start = end
        assert href, "expected a next-page href with cursor= but found none"
        params = parse_qs(urlparse(href).query)
        url = "/db/page_db/c?" + "&".join(f"{k}={v[0]}" for k, v in params.items())
    # The last page contains _id 10 and 11 (12 docs / page_size 5 = 3 pages).
    r = await http.get(url, headers={HEADER_NAME: "testtoken"})
    assert "Next page" not in r.text


async def test_collection_viewer_filter_invalid_json_shows_error(
    http: AsyncClient,
) -> None:
    r = await http.get("/db/x/c?filter=not-json", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Filter is not valid JSON" in r.text


async def test_collection_viewer_filter_with_id_rejected(server, http: AsyncClient) -> None:
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


async def test_collection_viewer_descending_sort(server, http: AsyncClient) -> None:
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


async def test_replace_doc_updates_and_returns_row(server, http: AsyncClient) -> None:
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


async def test_replace_doc_invalid_json_returns_modal_error(server, http: AsyncClient) -> None:
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


async def test_delete_confirm_modal_includes_typed_check(server, http: AsyncClient) -> None:
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
    # The user types the doc's _id value to confirm; UI gates the button
    # on it via Alpine. Server-side test: just verify the contract.
    assert "things" in r.text  # collection name still shown for context
    assert "Type the <code>_id</code> value" in r.text
    assert "hx-delete" in r.text
    assert "confirm !== '1'" in r.text  # Alpine guard wired up against _id


async def _ensure_test_indexes(server, db_name: str, coll_name: str) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc[db_name][coll_name]
        coll.insert_many([{"_id": i, "name": f"r-{i}", "tags": ["a"]} for i in range(5)])
        coll.create_index([("name", 1)], unique=True)
        coll.create_index([("tags", 1)])  # multikey on insert
        coll.create_index(
            [("created", 1)],
            partialFilterExpression={"name": "r-1"},
        )
    finally:
        mc.close()


# ---- indexes page (Slice 3.1) ----------------------------------------------


async def test_indexes_page_lists_with_badges(server, http: AsyncClient) -> None:
    await _ensure_test_indexes(server, "ix_db", "things")
    r = await http.get("/db/ix_db/things/indexes", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "_id_" in r.text
    assert "name_1" in r.text
    assert "tags_1" in r.text
    assert "created_1" in r.text
    assert "unique" in r.text
    assert "multikey" in r.text
    assert "partial" in r.text
    # _id_ row has no Drop button.
    assert "indexes/_id_/drop-confirm" not in r.text


# ---- create / drop index (Slice 3.2) ---------------------------------------


async def test_create_index_then_appears_on_listing(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["create_ix_db"]["things"].insert_many([{"_id": i, "name": f"r-{i}"} for i in range(3)])
    finally:
        mc.close()

    r = await http.post(
        "/db/create_ix_db/things/indexes",
        data={"key": '{"name": 1}', "unique": "true"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code in (200, 303)

    r2 = await http.get("/db/create_ix_db/things/indexes", headers={HEADER_NAME: "testtoken"})
    assert "name_1" in r2.text
    assert "unique" in r2.text


async def test_create_index_invalid_key_returns_400(http: AsyncClient) -> None:
    r = await http.post(
        "/db/whatever/things/indexes",
        data={"key": "not-json"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400


async def test_drop_confirm_modal_includes_typed_check(server, http: AsyncClient) -> None:
    await _ensure_test_indexes(server, "drop_ix_db", "things")
    r = await http.get(
        "/db/drop_ix_db/things/indexes/name_1/drop-confirm",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "name_1" in r.text
    assert "Type the index name" in r.text


async def test_drop_id_index_refused(http: AsyncClient) -> None:
    r = await http.get(
        "/db/x/c/indexes/_id_/drop-confirm",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400


async def test_drop_index_endpoint_removes_it(server, http: AsyncClient) -> None:
    await _ensure_test_indexes(server, "drop_ep_db", "things")
    r = await http.delete(
        "/db/drop_ep_db/things/indexes/name_1",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    r2 = await http.get("/db/drop_ep_db/things/indexes", headers={HEADER_NAME: "testtoken"})
    assert "name_1" not in r2.text


# ---- explain visualizer (Slice 3.3) ----------------------------------------


async def test_explain_renders_collscan_for_unindexed_filter(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["explain_db"]["c"].insert_many([{"_id": i, "x": i} for i in range(3)])
    finally:
        mc.close()

    r = await http.get(
        '/db/explain_db/c/explain?filter={"x": 1}',
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "COLLSCAN" in r.text
    assert "Winning plan" in r.text


async def test_explain_renders_ixscan_when_index_covers_query(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["explain_ix_db"]["c"]
        coll.insert_many([{"_id": i, "x": i} for i in range(5)])
        coll.create_index([("x", 1)])
    finally:
        mc.close()

    r = await http.get(
        '/db/explain_ix_db/c/explain?filter={"x": 3}',
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "IXSCAN" in r.text
    assert "x_1" in r.text
    assert "FETCH" in r.text


async def test_explain_invalid_json_filter_shows_error(http: AsyncClient) -> None:
    r = await http.get(
        "/db/x/c/explain?filter=not-json",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "Filter is not valid JSON" in r.text


# ---- /ws/metrics WebSocket (Slice 4) ---------------------------------------


def test_ws_metrics_streams_backlog_and_tick(server, tmp_path) -> None:
    """Sync test using starlette TestClient — websocket support is sync.

    Drives the websocket through the FastAPI lifespan so the sampler
    actually starts. We poke the sampler directly to deterministically
    produce ticks rather than waiting on the 1Hz timer.
    """
    from fastapi.testclient import TestClient

    app = create_app(mongo_uri=server.uri, token="ws-token", history_path=tmp_path / "h.db")
    with TestClient(app) as client:
        # Lifespan just started the sampler; force one synchronous tick
        # before connecting so backlog has something in it.
        app.state.sampler.tick_once()

        with client.websocket_connect("/ws/metrics?t=ws-token") as ws:
            backlog = ws.receive_json()
            assert backlog["type"] == "backlog"
            assert isinstance(backlog["samples"], list)
            # Trigger another tick after subscribing, expect a streamed frame.
            app.state.sampler.tick_once()
            tick = ws.receive_json()
            assert tick["type"] == "tick"
            sample = tick["sample"]
            assert "uptime" in sample
            assert "delta" in sample
            assert "opcounters" in sample


def test_ws_metrics_rejects_missing_token(server, tmp_path) -> None:
    from fastapi.testclient import TestClient
    from starlette.websockets import WebSocketDisconnect

    app = create_app(mongo_uri=server.uri, token="ws-token", history_path=tmp_path / "h.db")
    with (
        TestClient(app) as client,
        pytest.raises(WebSocketDisconnect),
        client.websocket_connect("/ws/metrics") as ws,
    ):
        ws.receive_json()


def test_ws_metrics_rejects_bad_token(server, tmp_path) -> None:
    from fastapi.testclient import TestClient
    from starlette.websockets import WebSocketDisconnect

    app = create_app(mongo_uri=server.uri, token="ws-token", history_path=tmp_path / "h.db")
    with (
        TestClient(app) as client,
        pytest.raises(WebSocketDisconnect),
        client.websocket_connect("/ws/metrics?t=wrong") as ws,
    ):
        ws.receive_json()


# ---- /users + /roles (Slice 5) ----------------------------------------------


async def test_roles_page_lists_built_in_roles(http: AsyncClient) -> None:
    r = await http.get("/roles", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    for role in ("read", "readWrite", "dbAdmin", "userAdmin", "root"):
        assert role in r.text
    # readAnyDatabase carries the admin_only flag.
    assert "admin_only" in r.text


async def test_users_page_renders_empty_admin_db(http: AsyncClient) -> None:
    r = await http.get("/users", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "No users on this database" in r.text


async def test_create_user_then_lists_with_role_badge(server, http: AsyncClient) -> None:
    r = await http.post(
        "/users?db=admin",
        data={
            "username": "alice",
            "password": "s3cret",
            "roles": ["readWrite@app", "read@admin"],
        },
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code in (200, 303)
    r2 = await http.get("/users?db=admin", headers={HEADER_NAME: "testtoken"})
    assert "alice" in r2.text
    assert "readWrite@app" in r2.text
    assert "read@admin" in r2.text


async def test_create_user_without_roles_rejected(http: AsyncClient) -> None:
    r = await http.post(
        "/users?db=admin",
        data={"username": "bob", "password": "x"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400


async def test_change_password_modal_renders(http: AsyncClient) -> None:
    # Modal is purely UI; doesn't actually need the user to exist for the
    # GET, since the form action is what fails on a non-existent user.
    r = await http.get(
        "/users/admin/missing/password",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "Change password" in r.text


async def test_change_password_mismatch_returns_modal_error(server, http: AsyncClient) -> None:
    # Create a user first so updateUser has somewhere to land.
    await http.post(
        "/users?db=admin",
        data={
            "username": "carol",
            "password": "p1",
            "roles": ["read@admin"],
        },
        headers={HEADER_NAME: "testtoken"},
    )
    r = await http.post(
        "/users/admin/carol/password",
        data={"password": "p1", "confirm": "p2"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "Passwords do not match" in r.text


async def test_roles_modal_pre_checks_current_bindings(server, http: AsyncClient) -> None:
    await http.post(
        "/users?db=admin",
        data={
            "username": "dan",
            "password": "x",
            "roles": ["read@admin"],
        },
        headers={HEADER_NAME: "testtoken"},
    )
    r = await http.get(
        "/users/admin/dan/roles",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    # The current "read@admin" checkbox must come back checked.
    assert (
        'value="read@admin"\n              checked' in r.text
        or 'value="read@admin" checked' in r.text
    )


async def test_update_roles_grants_and_revokes(server, http: AsyncClient) -> None:
    # Start: read@admin only.
    await http.post(
        "/users?db=admin",
        data={
            "username": "eve",
            "password": "x",
            "roles": ["read@admin"],
        },
        headers={HEADER_NAME: "testtoken"},
    )
    # Replace: readWrite@app only.
    r = await http.post(
        "/users/admin/eve/roles",
        data={"roles": ["readWrite@app"]},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    # Confirm via the listing.
    r2 = await http.get("/users?db=admin", headers={HEADER_NAME: "testtoken"})
    # eve now has readWrite@app, no more read@admin.
    body = r2.text
    eve_block = body.split("user-eve", 1)[-1].split("</tr>", 1)[0]
    assert "readWrite@app" in eve_block
    assert "read@admin" not in eve_block


async def test_changestream_page_renders(http: AsyncClient) -> None:
    r = await http.get("/changestream", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Change stream" in r.text
    # Scope picker is wired up.
    assert 'name="scope"' in r.text


def test_ws_changes_streams_collection_event(server, tmp_path) -> None:
    """Open a coll-scope tail, write a doc, expect a single event frame."""
    from fastapi.testclient import TestClient
    from pymongo import MongoClient

    app = create_app(mongo_uri=server.uri, token="cs-token", history_path=tmp_path / "h.db")
    with (
        TestClient(app) as client,
        client.websocket_connect("/ws/changes/coll?t=cs-token&db=cs_db&coll=c") as ws,
    ):
        opened = ws.receive_json()
        assert opened["type"] == "open"
        assert opened["namespace"] == "cs_db.c"
        # Insert a doc; expect an "insert" event to come through.
        mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            mc["cs_db"]["c"].insert_one({"_id": 1, "x": 1})
        finally:
            mc.close()
        evt = ws.receive_json()
        assert evt["type"] == "event"
        assert evt["event"]["operationType"] == "insert"
        assert evt["event"]["ns"]["db"] == "cs_db"
        assert evt["event"]["ns"]["coll"] == "c"


def test_ws_changes_rejects_missing_token(server, tmp_path) -> None:
    from fastapi.testclient import TestClient
    from starlette.websockets import WebSocketDisconnect

    app = create_app(mongo_uri=server.uri, token="cs-token", history_path=tmp_path / "h.db")
    with (
        TestClient(app) as client,
        pytest.raises(WebSocketDisconnect),
        client.websocket_connect("/ws/changes/coll?db=x&coll=y") as ws,
    ):
        ws.receive_json()


def test_ws_changes_rejects_bad_scope(server, tmp_path) -> None:
    from fastapi.testclient import TestClient
    from starlette.websockets import WebSocketDisconnect

    app = create_app(mongo_uri=server.uri, token="cs-token", history_path=tmp_path / "h.db")
    with (
        TestClient(app) as client,
        pytest.raises(WebSocketDisconnect),
        client.websocket_connect("/ws/changes/bogus?t=cs-token") as ws,
    ):
        ws.receive_json()


def test_ws_changes_coll_scope_requires_db_and_coll(server, tmp_path) -> None:
    from fastapi.testclient import TestClient
    from starlette.websockets import WebSocketDisconnect

    app = create_app(mongo_uri=server.uri, token="cs-token", history_path=tmp_path / "h.db")
    with (
        TestClient(app) as client,
        pytest.raises(WebSocketDisconnect),
        client.websocket_connect("/ws/changes/coll?t=cs-token") as ws,
    ):
        ws.receive_json()


async def test_drop_user_endpoint_removes_user(server, http: AsyncClient) -> None:
    await http.post(
        "/users?db=admin",
        data={
            "username": "frank",
            "password": "x",
            "roles": ["read@admin"],
        },
        headers={HEADER_NAME: "testtoken"},
    )
    r = await http.delete(
        "/users/admin/frank",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    r2 = await http.get("/users?db=admin", headers={HEADER_NAME: "testtoken"})
    assert "frank" not in r2.text


async def test_delete_doc_removes_and_returns_empty(server, http: AsyncClient) -> None:
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


# ---- /query (Slice 7) ----------------------------------------------------


async def test_console_page_renders_with_tabs(http: AsyncClient) -> None:
    r = await http.get("/query", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    for tab in ("find", "aggregate", "runCommand"):
        assert tab in r.text


@pytest.mark.parametrize("path", ["/query", "/changestream"])
async def test_alpine_x_data_attribute_is_well_formed(http: AsyncClient, path: str) -> None:
    """The Alpine ``x-data`` attribute on these pages embeds a JSON literal.

    Regression for a template-quoting bug: writing
    ``x-data="someFn({{ ctx | tojson }})"`` rendered to
    ``x-data="someFn({"key": "value"})"`` — the embedded ``"`` from the
    JSON terminated the attribute prematurely, so Alpine never received
    a valid initialiser, and every ``x-show`` / ``@click`` /
    ``:class`` directive on the page silently no-op'd. The console
    page's tab buttons looked clickable but did nothing; the
    changestream page never initialised its WebSocket.

    This test asserts the rendered ``x-data`` attribute value parses
    back as a valid ``fn(<json>)`` expression, which is what Alpine
    actually evaluates. Using Python's stdlib HTML parser means we
    catch any malformed-attribute regression regardless of which side
    of the quoting (single vs. double, escaping, etc.) the fix lives
    on.
    """
    import json
    from html.parser import HTMLParser

    r = await http.get(path, headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200

    found: list[str] = []

    class _XDataExtractor(HTMLParser):
        def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
            for name, value in attrs:
                if name == "x-data" and value is not None:
                    found.append(value)

    parser = _XDataExtractor()
    parser.feed(r.text)

    assert found, f"no x-data attribute found in {path} response"
    for value in found:
        # Expected shape: ``someFn({...json...})``. Strip the function
        # wrapper and feed the inside to json.loads — broken attribute
        # parsing would have truncated the JSON mid-string.
        assert value.endswith(")"), (
            f"{path}: x-data attribute looks truncated (no closing paren): {value!r}"
        )
        open_paren = value.index("(")
        json_payload = value[open_paren + 1 : -1]
        try:
            json.loads(json_payload)
        except json.JSONDecodeError as exc:
            raise AssertionError(
                f"{path}: x-data initialiser JSON failed to parse "
                f"({exc}); rendered value was {value!r}"
            ) from exc


async def test_console_find_returns_docs(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["console_db"]["c"].insert_many([{"_id": i, "name": f"row-{i}"} for i in range(3)])
    finally:
        mc.close()

    r = await http.post(
        "/query/find",
        data={
            "db": "console_db",
            "coll": "c",
            "filter": "{}",
            "sort": "",
            "projection": "",
            "limit": "5",
        },
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "row-0" in r.text
    assert "row-2" in r.text
    assert "Results" in r.text
    # Recorded in the recent panel.
    assert "find #" in r.text


async def test_console_find_invalid_filter_renders_error(
    http: AsyncClient,
) -> None:
    r = await http.post(
        "/query/find",
        data={
            "db": "x",
            "coll": "c",
            "filter": "not-json",
            "sort": "",
            "projection": "",
            "limit": "5",
        },
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "Filter is not valid" in r.text


async def test_console_aggregate_returns_docs(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["agg_db"]["c"].insert_many([{"_id": i} for i in range(5)])
    finally:
        mc.close()

    r = await http.post(
        "/query/aggregate",
        data={
            "db": "agg_db",
            "coll": "c",
            "pipeline": '[{"$count": "n"}]',
            "limit": "10",
        },
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    # $count returns one doc {n: 5}; HTML escapes the quotes.
    assert "n" in r.text and "5" in r.text


async def test_console_run_command_returns_response(http: AsyncClient) -> None:
    r = await http.post(
        "/query/runCommand",
        data={"db": "admin", "command": '{"ping": 1}'},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    # ping returns {ok: 1.0}; the HTML-escaped JSON spelling is &#34;ok&#34;.
    assert "ok" in r.text and "1.0" in r.text


async def test_console_history_endpoint_returns_payload(
    http: AsyncClient,
) -> None:
    # Submit a runCommand so the history has at least one entry.
    await http.post(
        "/query/runCommand",
        data={"db": "admin", "command": '{"ping": 1}'},
        headers={HEADER_NAME: "testtoken"},
    )
    # Pull the rendered page to find the most recent entry id.
    r = await http.get("/query", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    import re

    match = re.search(r"loadHistory\((\d+),\s*'(runCommand|find|aggregate)'\)", r.text)
    assert match is not None
    entry_id = int(match.group(1))
    r2 = await http.get(f"/query/history/{entry_id}", headers={HEADER_NAME: "testtoken"})
    assert r2.status_code == 200
    payload = r2.json()
    assert payload["db"] == "admin"
    assert payload["command"] == '{"ping": 1}'


# ---- /connections + /cursors (Slice 8) -------------------------------------


async def test_connections_page_lists_caller(http: AsyncClient) -> None:
    r = await http.get("/connections", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # The pymongo client backing the facade is itself a connection in
    # currentOp, so its (host:port) renders in the table.
    assert "127.0.0.1:" in r.text


async def test_cursors_page_lists_open_cursor(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["cursors_db"]["c"]
        coll.insert_many([{"_id": i} for i in range(20)])
        # Open and partially drain a cursor so it stays open server-side.
        cursor = coll.find().batch_size(2)
        next(cursor)
        try:
            r = await http.get("/cursors", headers={HEADER_NAME: "testtoken"})
            assert r.status_code == 200
            assert "cursors_db.c" in r.text
        finally:
            cursor.close()
    finally:
        mc.close()


async def test_kill_cursor_confirm_modal_renders(http: AsyncClient) -> None:
    r = await http.get(
        "/cursors/12345/kill-confirm?ns=db.coll",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "Type the namespace" in r.text
    assert "db.coll" in r.text  # ns is the typed-confirm target
    assert "12345" in r.text  # cursor id still shown for context


async def test_kill_cursor_endpoint_kills(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["kill_curs_db"]["c"]
        coll.insert_many([{"_id": i} for i in range(20)])
        cursor = coll.find().batch_size(2)
        next(cursor)
        cursor_id = cursor.cursor_id
        try:
            r = await http.delete(
                f"/cursors/{cursor_id}?ns=kill_curs_db.c",
                headers={HEADER_NAME: "testtoken"},
            )
            assert r.status_code == 200
        finally:
            # The pymongo cursor was force-killed by our endpoint;
            # cursor.close() is a no-op now.
            cursor.close()
    finally:
        mc.close()


async def test_kill_cursor_requires_ns(http: AsyncClient) -> None:
    r = await http.delete(
        "/cursors/12345",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400


# ---- /profiler (Slice 9) ---------------------------------------------------


async def test_profiler_page_default_state(http: AsyncClient) -> None:
    r = await http.get("/profiler", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # Off by default.
    assert "0 — off" in r.text
    # Defaults pre-filled.
    assert 'value="100"' in r.text


async def test_profiler_post_changes_state(http: AsyncClient) -> None:
    r = await http.post(
        "/profiler?db=app",
        data={"level": "2", "slowms": "75", "sample_rate": "0.5"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code in (200, 303)
    r2 = await http.get("/profiler?db=app", headers={HEADER_NAME: "testtoken"})
    assert r2.status_code == 200
    # Level 2 selected.
    assert 'value="2" selected' in r2.text
    assert 'value="75"' in r2.text


async def test_profiler_lists_entries_after_op(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        # Arm level 2 then do an op so system.profile has at least one entry.
        mc["entries_db"].command("profile", 2)
        mc["entries_db"]["c"].insert_one({"_id": 1, "x": 1})
    finally:
        mc.close()

    r = await http.get("/profiler?db=entries_db", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # The insert op shows up.
    assert "insert" in r.text
    assert "entries_db.c" in r.text


async def test_profiler_post_invalid_level_400(http: AsyncClient) -> None:
    r = await http.post(
        "/profiler?db=app",
        data={"level": "3", "slowms": "100", "sample_rate": "1.0"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400


# ---- /maintenance (Slice 10) ------------------------------------------------


async def test_maintenance_page_renders(http: AsyncClient) -> None:
    r = await http.get("/maintenance", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    for label in ("Force checkpoint", "Prune oplog", "Prune TTL", "Danger zone"):
        assert label in r.text


async def test_maintenance_fsync_runs_and_flashes(http: AsyncClient) -> None:
    r = await http.post("/maintenance/fsync", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "fsync ok" in r.text


async def test_maintenance_prune_oplog_returns_count(http: AsyncClient) -> None:
    r = await http.post("/maintenance/prune-oplog", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "pruned" in r.text and "oplog row" in r.text


async def test_maintenance_prune_ttl_returns_count(http: AsyncClient) -> None:
    r = await http.post("/maintenance/prune-ttl", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "TTL doc" in r.text


async def test_maintenance_drop_db_modal_typed_check(http: AsyncClient) -> None:
    r = await http.get(
        "/maintenance/drop-database/myapp/confirm",
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "Type the database name" in r.text
    assert "myapp" in r.text


async def test_maintenance_drop_db_actually_drops(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["killme"]["c"].insert_one({"_id": 1})
        assert "killme" in mc.list_database_names()
    finally:
        mc.close()

    r = await http.post(
        "/maintenance/drop-database",
        data={"db": "killme"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "dropped database killme" in r.text

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        assert "killme" not in mc.list_database_names()
    finally:
        mc.close()


# ---- /db/{db}/{coll}/schema, /logs, /db/{db}/{coll}/geo (Slice 11) ---------


async def test_schema_page_summarises_fields(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["sdb"]["c"].insert_many(
            [{"_id": i, "name": f"row-{i}", "tags": ["a"]} for i in range(5)]
        )
    finally:
        mc.close()

    r = await http.get("/db/sdb/c/schema?sample_size=10", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # All three top-level paths should appear.
    assert "name" in r.text
    assert "tags" in r.text
    # Type badges render.
    assert "string" in r.text
    assert "array" in r.text


async def test_logs_page_renders_and_partial_returns_lines(server, http: AsyncClient) -> None:
    # Drop a known marker into the in-memory log via the storage handle.
    server.logs.append("I", "TEST", "schema-test-marker")
    r = await http.get("/logs", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Refreshes every 2 seconds" in r.text
    r2 = await http.get("/_partials/logs", headers={HEADER_NAME: "testtoken"})
    assert r2.status_code == 200
    assert "schema-test-marker" in r2.text


async def test_geo_page_renders_empty_when_no_geo_index(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["geo_db"]["plain"].insert_one({"_id": 1, "name": "x"})
    finally:
        mc.close()

    r = await http.get("/db/geo_db/plain/geo", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "No" in r.text and "2dsphere" in r.text


# ---- /backup (Slice 12) ----------------------------------------------------


async def test_backup_page_renders(http: AsyncClient) -> None:
    r = await http.get("/backup", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Run mongodump now" in r.text
    assert "Existing backups" in r.text


async def test_backup_lists_existing_backups(app, http: AsyncClient) -> None:
    # Drop a fake backup directory under the per-test backup_root.
    root = app.state.backup_root
    root.mkdir(parents=True, exist_ok=True)
    (root / "20260101T000000Z").mkdir()
    r = await http.get("/backup", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "20260101T000000Z" in r.text


async def test_backup_restore_rejects_traversal(http: AsyncClient) -> None:
    r = await http.post(
        "/backup/restore",
        data={"name": "../etc"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "invalid backup name" in r.text


async def test_backup_restore_archive_rejects_traversal(http: AsyncClient) -> None:
    """The native-archive restore form rejects path traversal in both
    the archive name and the target dir."""
    r = await http.post(
        "/backup/restore-archive",
        data={"name": "../etc.tar.gz", "target_dir": "/tmp/x"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "invalid archive name" in r.text
    r = await http.post(
        "/backup/restore-archive",
        data={"name": "archive-x.tar.gz", "target_dir": "/etc/../etc"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "invalid target directory" in r.text


async def test_backup_restore_archive_extracts_into_target(
    server, app, http: AsyncClient, tmp_path
) -> None:
    """End-to-end: archive a backup, post the restore-archive form,
    a new server reads the snapshot from the target dir."""
    from pymongo import MongoClient

    from secantus import SecantusDBServer

    backup_root = app.state.backup_root
    backup_root.mkdir(parents=True, exist_ok=True)
    archive = backup_root / "archive-routesmoke.tar.gz"
    target = tmp_path / "restored-from-route"

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["routedb"]["coll"].insert_one({"_id": 1, "v": "route-smoke"})
        mc.admin.command("secantusAdmin.backupArchive", outputPath=str(archive))
    finally:
        mc.close()

    r = await http.post(
        "/backup/restore-archive",
        data={"name": archive.name, "target_dir": str(target)},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "restoreArchive" in r.text
    assert str(target) in r.text
    assert target.is_dir()
    assert (target / "WiredTiger").is_file()

    srv2 = SecantusDBServer(port=0, storage_path=str(target))
    srv2.start()
    try:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            assert list(c2["routedb"]["coll"].find()) == [{"_id": 1, "v": "route-smoke"}]
        finally:
            c2.close()
    finally:
        srv2.stop()


async def test_backup_page_shows_extract_button_for_tar_gz(server, app, http: AsyncClient) -> None:
    """The Existing backups table renders an Extract control for
    ``.tar.gz`` archives and a regular Restore button for directories."""
    backup_root = app.state.backup_root
    backup_root.mkdir(parents=True, exist_ok=True)
    (backup_root / "20260101T000000Z").mkdir()
    (backup_root / "archive-uitest.tar.gz").write_bytes(b"\x1f\x8b" + b"x" * 16)
    r = await http.get("/backup", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    body = r.text
    assert "archive-uitest.tar.gz" in body
    assert "20260101T000000Z" in body
    # Extract control points at the new route; mongodump row keeps
    # the old route.
    assert "/backup/restore-archive" in body
    assert "/backup/restore" in body  # still there for the dir row


async def test_geo_page_renders_with_2dsphere_index(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["geo_db2"]["places"]
        coll.create_index([("loc", "2dsphere")])
        coll.insert_many(
            [
                {
                    "_id": i,
                    "loc": {"type": "Point", "coordinates": [0.1 * i, 0.1 * i]},
                }
                for i in range(3)
            ]
        )
    finally:
        mc.close()

    r = await http.get("/db/geo_db2/places/geo", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Geometry field" in r.text
    assert "loc" in r.text
    # Features were serialized into the page.
    assert '"type": "Point"' in r.text or "type: 'Point'" in r.text


async def test_maintenance_drop_coll_actually_drops(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["dcdb"]["go"].insert_one({"_id": 1})
        mc["dcdb"]["stay"].insert_one({"_id": 1})
    finally:
        mc.close()

    r = await http.post(
        "/maintenance/drop-collection",
        data={"db": "dcdb", "coll": "go"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "dropped collection dcdb.go" in r.text

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        names = set(mc["dcdb"].list_collection_names())
        assert "go" not in names
        assert "stay" in names
    finally:
        mc.close()


# ---- /query datalists + collections endpoint --------------------------------


async def test_query_page_lists_databases_in_datalist(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["alpha"]["c"].insert_one({"_id": 1})
        mc["beta"]["c"].insert_one({"_id": 1})
    finally:
        mc.close()

    r = await http.get("/query", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # The datalist contains every database we just seeded.
    assert 'value="alpha"' in r.text
    assert 'value="beta"' in r.text


async def test_query_collections_endpoint_returns_names(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["zoo"]["lions"].insert_one({"_id": 1})
        mc["zoo"]["tigers"].insert_one({"_id": 1})
    finally:
        mc.close()

    r = await http.get("/query/_collections?db=zoo", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    payload = r.json()
    assert sorted(payload["collections"]) == ["lions", "tigers"]


async def test_query_collections_endpoint_unknown_db_empty(
    http: AsyncClient,
) -> None:
    r = await http.get("/query/_collections?db=", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert r.json()["collections"] == []


async def test_query_runcommand_blank_renders_error_inline(
    http: AsyncClient,
) -> None:
    """Posting an empty runCommand form must NOT 422 with raw JSON —
    the handler returns the page with an inline error so the user can
    fix it without losing the chrome."""
    r = await http.post(
        "/query/runCommand",
        data={"db": "", "command": ""},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "Database is required" in r.text
    assert "Command is required" in r.text
    # Page chrome (sidebar) is still there.
    assert "Dashboard" in r.text
    assert '"detail"' not in r.text


async def test_query_aggregate_blank_renders_error_inline(
    http: AsyncClient,
) -> None:
    r = await http.post(
        "/query/aggregate",
        data={"db": "", "coll": "", "pipeline": ""},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "Database is required" in r.text
    assert "Collection is required" in r.text
    assert "Pipeline is required" in r.text


async def test_global_validation_handler_renders_back_page(
    http: AsyncClient,
) -> None:
    """A POST that's missing a Form field FastAPI considers required
    should hit our exception handler, not the raw 422 JSON page."""
    r = await http.post(
        "/maintenance/drop-database",
        data={},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "Missing required field" in r.text or "Form validation" in r.text
    assert "← Back" in r.text


async def test_dashboard_badge_says_embedded_when_target_is_embedded(server, tmp_path) -> None:
    """Once the user starts the embedded server (which auto-swaps the
    target), the page-header badge should say 'Embedded SecantusDB'
    rather than the kernel-assigned port."""
    app = create_app(
        mongo_uri=server.uri,
        token="testtoken",
        history_path=tmp_path / "hist.db",
        embedded_storage=tmp_path / "emb",
    )
    try:
        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://testserver",
        ) as c:
            r0 = await c.get("/", headers={HEADER_NAME: "testtoken"})
            # The badge starts as a regular URI badge — no embedded class.
            assert "server-badge-embedded" not in r0.text

            r1 = await c.post(
                "/embedded/start",
                data={"storage_path": ""},
                headers={HEADER_NAME: "testtoken"},
            )
            assert r1.status_code == 200
            assert "server-badge-embedded" in r1.text
    finally:
        app.state.embedded.stop()
        app.state.mongo.close()


# ---- /server embedded-server widget -----------------------------------------


async def test_server_page_renders_embedded_widget_when_stopped(
    http: AsyncClient,
) -> None:
    """The embedded-server controls live on the /server tab (alongside
    target switching), not on the dashboard."""
    r = await http.get("/server", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Embedded SecantusDB server" in r.text
    assert "stopped" in r.text


async def test_dashboard_no_longer_shows_embedded_widget(
    http: AsyncClient,
) -> None:
    """Reverse of the above: the dashboard is now metrics-only."""
    r = await http.get("/", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "Embedded SecantusDB server" not in r.text


async def test_embedded_start_stop_round_trip(server, tmp_path) -> None:
    """Starting the embedded server boots an in-process listener and
    swaps the admin app's target to it."""
    app = create_app(
        mongo_uri=server.uri,
        token="testtoken",
        history_path=tmp_path / "hist.db",
        embedded_storage=tmp_path / "embed",
    )
    try:
        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://testserver",
        ) as c:
            r = await c.post(
                "/embedded/start",
                data={"storage_path": ""},
                headers={HEADER_NAME: "testtoken"},
            )
            assert r.status_code == 200
            assert "Started embedded server" in r.text

            # The admin app's target now points at the embedded URI.
            embedded_uri = app.state.embedded.status()["uri"]
            assert embedded_uri is not None
            assert app.state.mongo_uri == embedded_uri

            # Stop again — page reflects "stopped".
            r2 = await c.post("/embedded/stop", headers={HEADER_NAME: "testtoken"})
            assert r2.status_code == 200
            assert "Stopped embedded server" in r2.text
            assert app.state.embedded.status()["running"] is False
    finally:
        app.state.embedded.stop()
        app.state.mongo.close()


# ---- /insert (slice 13) -----------------------------------------------------


async def test_insert_page_renders(http: AsyncClient) -> None:
    r = await http.get("/insert", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert 'action="/insert"' in r.text
    assert "Document(s)" in r.text


async def test_insert_link_in_sidebar_below_query(http: AsyncClient) -> None:
    r = await http.get("/", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    body = r.text
    qi = body.find('href="/query"')
    ii = body.find('href="/insert"')
    di = body.find('href="/db"')
    assert qi != -1 and ii != -1 and di != -1
    # Insert sits between Query and Databases.
    assert qi < ii < di


async def test_insert_single_document_round_trip(server, http: AsyncClient) -> None:
    r = await http.post(
        "/insert",
        data={"db": "tdb", "coll": "things", "docs": '{"x": 1, "name": "alpha"}'},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "Inserted" in r.text
    assert "1 doc" in r.text
    # Sanity: real document landed in the underlying SecantusDB.
    import pymongo

    with pymongo.MongoClient(server.uri, serverSelectionTimeoutMS=2000) as c:
        n = c["tdb"]["things"].count_documents({"x": 1, "name": "alpha"})
    assert n == 1


async def test_insert_array_payload(server, http: AsyncClient) -> None:
    r = await http.post(
        "/insert",
        data={"db": "tdb2", "coll": "items", "docs": '[{"a": 1}, {"a": 2}, {"a": 3}]'},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "3 docs" in r.text


async def test_insert_ndjson_payload(server, http: AsyncClient) -> None:
    payload = "\n".join(['{"k": 1}', '{"k": 2}', "", '{"k": 3}'])
    r = await http.post(
        "/insert",
        data={"db": "tdb3", "coll": "rows", "docs": payload},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 200
    assert "3 docs" in r.text


async def test_insert_missing_fields_renders_inline_errors(http: AsyncClient) -> None:
    r = await http.post(
        "/insert",
        data={"db": "", "coll": "", "docs": ""},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    body = r.text
    assert "Database is required" in body
    assert "Collection is required" in body
    assert "Document(s) field is required" in body


async def test_insert_invalid_json_returns_400_not_500(http: AsyncClient) -> None:
    r = await http.post(
        "/insert",
        data={"db": "tdb", "coll": "items", "docs": "{not real json"},
        headers={HEADER_NAME: "testtoken"},
    )
    assert r.status_code == 400
    assert "valid Extended JSON" in r.text or "JSON object" in r.text


# ---- CLI surfaces a fix-it message when the admin extra is missing ---------


def test_cli_missing_admin_extra_shows_helpful_message(monkeypatch, capsys) -> None:
    """When fastapi/uvicorn aren't installed the CLI must point the user
    at the right install command — not raise ``ModuleNotFoundError``."""

    from secantus.admin import cli as admin_cli

    real_import = __import__

    def faux_import(name, *args, **kwargs):
        if name == "secantus.admin.launcher":
            raise ModuleNotFoundError("No module named 'uvicorn'", name="uvicorn")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", faux_import)

    rc = admin_cli.main(["--no-window"])
    assert rc == 1
    captured = capsys.readouterr()
    err = captured.err
    assert "admin' extra" in err
    assert "uvicorn" in err
    assert "pip install 'secantusdb[admin]'" in err


async def test_json_pretty_script_loaded_on_every_page(http: AsyncClient) -> None:
    """The pretty-printer script is loaded from base.html so every page
    benefits from token highlighting on <pre class='doc-body'>."""
    r = await http.get("/", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    assert "/static/js/json-pretty.js" in r.text


async def test_json_pretty_static_file_served(http: AsyncClient) -> None:
    """The script itself is reachable as a static asset."""
    r = await http.get("/static/js/json-pretty.js", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    body = r.text
    # Public helpers the changestream page (and any future Alpine page)
    # depends on.
    assert "secantusFormatJsonHtml" in body
    assert "secantusPrettyJson" in body
    # HTML-escape pass before tokenisation guards against XSS in stored
    # documents — a doc with "<script>" in a string field must not turn
    # into a real script tag in the rendered page.
    assert "escapeHtml" in body
