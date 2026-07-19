"""PITR panel on /backup — base snapshot + recover-to-a-point-in-time.

Drives the real Python server over the wire (no mocks): the admin routes
issue ``secantusAdmin.archiveBaseSnapshot`` / ``restoreToTimestamp`` and
the assertions are about what actually landed on disk.
"""

from __future__ import annotations

import pytest
from httpx import ASGITransport, AsyncClient

from secantus import SecantusDBServer
from secantus.admin import capabilities, create_app
from secantus.admin.middleware import HEADER_NAME

pytestmark = pytest.mark.anyio

_AUTH = {HEADER_NAME: "testtoken"}


@pytest.fixture
def anyio_backend():
    return "asyncio"


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "data")) as srv:
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


async def test_pitr_panel_shown_on_secantus(app, http: AsyncClient) -> None:
    r = await http.get("/backup", headers=_AUTH)
    assert r.status_code == 200
    assert "Point-in-time recovery" in r.text
    assert 'action="/backup/pitr/snapshot"' in r.text
    assert 'action="/backup/pitr/restore"' in r.text


async def test_pitr_panel_hidden_on_mongodb(app, http: AsyncClient) -> None:
    # Proprietary commands — a real mongod definitionally can't serve them.
    app.state.capabilities = capabilities.classify({"version": "7.0.5"}, {})
    r = await http.get("/backup", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/backup/pitr/snapshot"' not in r.text
    assert "doesn't serve" in r.text


async def test_base_snapshot_writes_an_archive(app, http: AsyncClient, tmp_path) -> None:
    archive_dir = tmp_path / "pitr"
    r = await http.post(
        "/backup/pitr/snapshot",
        data={"archive_dir": str(archive_dir)},
        headers=_AUTH,
    )
    assert r.status_code == 200
    assert "Base snapshot →" in r.text, r.text[:2000]
    written = list(archive_dir.glob("base-*.tar.gz"))
    assert written, f"no base snapshot in {archive_dir}: {list(archive_dir.iterdir())}"


async def test_restore_to_timestamp_produces_a_target_dir(
    app, http: AsyncClient, tmp_path, server
) -> None:
    # Seed a document so the replay has something to apply.
    import pymongo

    client = pymongo.MongoClient(server.uri, directConnection=True)
    client["shop"]["orders"].insert_one({"_id": 1, "item": "widget"})
    client.close()

    archive_dir = tmp_path / "pitr"
    snap = await http.post(
        "/backup/pitr/snapshot", data={"archive_dir": str(archive_dir)}, headers=_AUTH
    )
    assert "Base snapshot →" in snap.text

    target = tmp_path / "recovered"
    r = await http.post(
        "/backup/pitr/restore",
        data={"source": str(archive_dir), "target_dir": str(target)},
        headers=_AUTH,
    )
    assert r.status_code == 200
    assert "Recovered to" in r.text, r.text[:2000]
    assert target.exists()
    # The message must tell the operator what to start — recovery is
    # offline-shaped and does not touch the running server.
    assert "--storage-path" in r.text


async def test_restore_rejects_unparseable_time(app, http: AsyncClient, tmp_path) -> None:
    r = await http.post(
        "/backup/pitr/restore",
        data={
            "source": str(tmp_path / "pitr"),
            "target_dir": str(tmp_path / "out"),
            "to_time": "last tuesday",
        },
        headers=_AUTH,
    )
    assert r.status_code == 200
    assert "Could not parse recovery time" in r.text


@pytest.mark.parametrize("field", ["source", "target_dir"])
async def test_restore_rejects_traversal(app, http: AsyncClient, tmp_path, field: str) -> None:
    payload = {"source": str(tmp_path / "pitr"), "target_dir": str(tmp_path / "out")}
    payload[field] = "../../etc"
    r = await http.post("/backup/pitr/restore", data=payload, headers=_AUTH)
    assert r.status_code == 200
    assert "invalid" in r.text.lower()
