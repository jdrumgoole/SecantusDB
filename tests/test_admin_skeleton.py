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
    r = await http.get("/_partials/dashboard-tiles", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    # Expect labels for the KPI grid we render today.
    for label in ("Uptime", "Connections", "Inserts", "Queries", "Wire requests"):
        assert label in r.text


async def test_dashboard_tiles_show_error_when_mongo_unreachable(tmp_path) -> None:
    # Point at a port nothing's listening on; the facade's ServerSelectionTimeoutError
    # surfaces as a translated MongoError → "Could not reach server" in the HTML.
    app = create_app(
        mongo_uri="mongodb://127.0.0.1:1",
        token="testtoken",
        history_path=tmp_path / "h.db",
    )
    try:
        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://testserver",
        ) as c:
            r = await c.get("/_partials/dashboard-tiles", headers={HEADER_NAME: "testtoken"})
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
        # Pull the next-page href if present.
        text = r.text
        if "Next page" not in text:
            break
        # Extract the cursor query parameter from the next-page anchor.
        start = text.find('href="/db/page_db/c?')
        end = text.find('"', start + 6)
        href = text[start + 6 : end].replace("&amp;", "&")
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
    # The user types the collection name to confirm; UI gates the button
    # on it via Alpine. Server-side test: just verify the contract.
    assert "things" in r.text  # collection name shown
    assert "Type the collection name" in r.text
    assert "hx-delete" in r.text
    assert "confirm !== 'things'" in r.text  # Alpine guard wired up


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


# ---- /console (Slice 7) ----------------------------------------------------


async def test_console_page_renders_with_tabs(http: AsyncClient) -> None:
    r = await http.get("/console", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    for tab in ("find", "aggregate", "runCommand"):
        assert tab in r.text


async def test_console_find_returns_docs(server, http: AsyncClient) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["console_db"]["c"].insert_many([{"_id": i, "name": f"row-{i}"} for i in range(3)])
    finally:
        mc.close()

    r = await http.post(
        "/console/find",
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
        "/console/find",
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
        "/console/aggregate",
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
        "/console/runCommand",
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
        "/console/runCommand",
        data={"db": "admin", "command": '{"ping": 1}'},
        headers={HEADER_NAME: "testtoken"},
    )
    # Pull the rendered page to find the most recent entry id.
    r = await http.get("/console", headers={HEADER_NAME: "testtoken"})
    assert r.status_code == 200
    import re

    match = re.search(r"loadHistory\((\d+),\s*'(runCommand|find|aggregate)'\)", r.text)
    assert match is not None
    entry_id = int(match.group(1))
    r2 = await http.get(f"/console/history/{entry_id}", headers={HEADER_NAME: "testtoken"})
    assert r2.status_code == 200
    payload = r2.json()
    assert payload["db"] == "admin"
    assert payload["command"] == '{"ping": 1}'
