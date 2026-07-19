"""Capability detection + UI feature-gating for the admin console.

The admin UI can point at the SecantusDB Python server, the Rust server,
or a real ``mongod``; ``secantus.admin.capabilities`` probes the target
and the templates gate feature buttons to what it supports.
"""

from __future__ import annotations

import pytest
from httpx import ASGITransport, AsyncClient

from secantus import SecantusDBServer
from secantus.admin import capabilities, create_app
from secantus.admin.client import MongoError
from secantus.admin.middleware import HEADER_NAME

# ---- unit: classify ---------------------------------------------------------


def test_classify_python_full_surface() -> None:
    caps = capabilities.classify(
        {"secantusVersion": "0.5.4b54", "version": "7.0.0"},
        {"secantus": {"server": "python", "version": "0.5.4b54"}},
    )
    assert caps.kind == "python"
    assert caps.is_secantus and caps.identified
    assert "Python" in caps.label and "0.5.4b54" in caps.label
    # Everything is supported.
    for flag in capabilities._FLAGS:
        assert getattr(caps, flag) is True


def test_classify_rust_is_permissive() -> None:
    # A SecantusDB target is never gated by a hardcoded per-flavour table.
    # The previous table claimed the Rust server lacked restoreArchive /
    # prune / grant-revoke / killOp / getLog / profile long after it had
    # implemented all six, which silently hid working buttons. Anything
    # the server genuinely lacks is learned from a live CommandNotFound
    # (see test_record_unsupported_*), never assumed up front.
    caps = capabilities.classify(
        {"secantusVersion": "0.5.2-beta.101"},
        {"secantus": {"server": "rust"}},
    )
    assert caps.kind == "rust"
    assert "Rust" in caps.label
    for flag in capabilities._FLAGS:
        assert getattr(caps, flag) is True, f"{flag} must not be assumed missing"


def test_classify_mongodb_no_native_commands() -> None:
    caps = capabilities.classify({"version": "7.0.5"}, {})
    assert caps.kind == "mongodb"
    assert caps.label == "MongoDB 7.0.5"
    # No secantusAdmin.* proprietary commands on a real mongod. These
    # negatives are definitional, not a porting snapshot, so they are the
    # one place a static profile is still correct.
    assert caps.native_backup_archive is False
    assert caps.native_restore_archive is False
    assert caps.native_prune is False
    assert caps.native_pitr is False
    # But every standard admin command is available:
    assert caps.grant_revoke_roles is True
    assert caps.kill_op is True
    assert caps.server_log is True
    assert caps.profiling is True


def test_classify_secantus_without_flavour_is_permissive() -> None:
    # buildInfo says SecantusDB but serverStatus didn't reveal python/rust —
    # treat as the full Python-level surface rather than hide a working button.
    caps = capabilities.classify({"secantusVersion": "0.5.4b54"}, {})
    assert caps.kind == "python"
    assert caps.native_prune is True


def test_unknown_is_permissive() -> None:
    caps = capabilities.UNKNOWN
    assert caps.kind == "unknown"
    assert caps.identified is False
    for flag in capabilities._FLAGS:
        assert getattr(caps, flag) is True


# ---- unit: probe ------------------------------------------------------------


class _FakeFacade:
    def __init__(self, build_info=None, server_status=None, fail=()):
        self._bi = build_info or {}
        self._ss = server_status or {}
        self._fail = fail

    def build_info(self):
        if "build_info" in self._fail:
            raise MongoError("boom")
        return self._bi

    def server_status(self):
        if "server_status" in self._fail:
            raise MongoError("boom")
        return self._ss


def test_probe_classifies_from_facade() -> None:
    facade = _FakeFacade(
        build_info={"secantusVersion": "0.5.2-beta.101"},
        server_status={"secantus": {"server": "rust"}},
    )
    caps = capabilities.probe(facade)
    assert caps.kind == "rust"


def test_probe_tolerates_one_failed_probe() -> None:
    # serverStatus restricted/failed but buildInfo works → still classifies.
    facade = _FakeFacade(build_info={"version": "7.0.5"}, fail=("server_status",))
    caps = capabilities.probe(facade)
    assert caps.kind == "mongodb"


def test_probe_raises_when_both_fail() -> None:
    facade = _FakeFacade(fail=("build_info", "server_status"))
    with pytest.raises(MongoError):
        capabilities.probe(facade)


# ---- unit: learned unsupported ----------------------------------------------


class _FakeApp:
    """Stands in for the FastAPI app — record_unsupported only needs .state."""

    def __init__(self, caps):
        self.state = type("S", (), {"capabilities": caps})()


def _RUST_CAPS():
    return capabilities.classify({"secantusVersion": "1"}, {"secantus": {"server": "rust"}})


def test_without_clears_one_flag() -> None:
    caps = capabilities.classify({"secantusVersion": "1"}, {"secantus": {"server": "rust"}})
    demoted = caps.without("native_prune")
    assert demoted.native_prune is False
    # Everything else survives, and the original is untouched (frozen).
    assert demoted.kill_op is True
    assert caps.native_prune is True


def test_without_rejects_unknown_flag() -> None:
    with pytest.raises(ValueError):
        capabilities.UNKNOWN.without("no_such_flag")


def test_record_unsupported_clears_flag_on_app() -> None:
    app = _FakeApp(_RUST_CAPS())
    assert app.state.capabilities.kill_op is True
    capabilities.record_unsupported(app, "kill_op")
    assert app.state.capabilities.kill_op is False


def test_record_unsupported_is_idempotent() -> None:
    app = _FakeApp(_RUST_CAPS())
    capabilities.record_unsupported(app, "server_log")
    first = app.state.capabilities
    capabilities.record_unsupported(app, "server_log")
    # Already cleared — no pointless object churn.
    assert app.state.capabilities is first


def test_is_command_not_found_only_matches_59() -> None:
    assert capabilities.is_command_not_found(MongoError("nope", code=59)) is True
    assert capabilities.is_command_not_found(MongoError("auth", code=13)) is False
    assert capabilities.is_command_not_found(MongoError("no code")) is False


# ---- integration: template gating -------------------------------------------

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


_AUTH = {HEADER_NAME: "testtoken"}

_MONGODB = capabilities.classify({"version": "7.0.5"}, {})
_RUST = capabilities.classify(
    {"secantusVersion": "0.5.2-beta.101"}, {"secantus": {"server": "rust"}}
)
_PYTHON = capabilities.classify({"secantusVersion": "0.5.4b54"}, {"secantus": {"server": "python"}})


async def test_app_default_capabilities_unknown(app) -> None:
    # Constructed without lifespan (test transport): permissive default.
    assert app.state.capabilities is capabilities.UNKNOWN


async def test_backup_native_button_hidden_on_mongodb(app, http: AsyncClient) -> None:
    app.state.capabilities = _MONGODB
    r = await http.get("/backup", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/backup/archive"' not in r.text
    assert "Native checkpoint backup unavailable" in r.text
    # mongodump path is wire-level and stays available.
    assert 'action="/backup/dump"' in r.text
    # And the server-type badge is shown.
    assert "MongoDB 7.0.5" in r.text


async def test_backup_native_button_shown_on_python(app, http: AsyncClient) -> None:
    app.state.capabilities = _PYTHON
    r = await http.get("/backup", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/backup/archive"' in r.text
    assert "Native checkpoint backup unavailable" not in r.text


async def test_backup_native_button_shown_when_unknown(app, http: AsyncClient) -> None:
    # Permissive default must not hide a working button.
    r = await http.get("/backup", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/backup/archive"' in r.text


async def test_maintenance_prune_shown_on_rust(app, http: AsyncClient) -> None:
    # Regression: the Rust server implements pruneOplog / pruneTtl, but a
    # stale hardcoded capability table claimed otherwise and hid both
    # buttons. A SecantusDB target must never have a feature hidden on
    # assumption alone.
    app.state.capabilities = _RUST
    r = await http.get("/maintenance", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/maintenance/prune-oplog"' in r.text
    assert 'action="/maintenance/prune-ttl"' in r.text
    assert "Prune oplog / TTL unavailable" not in r.text


async def test_maintenance_prune_hidden_on_mongodb(app, http: AsyncClient) -> None:
    # A real mongod definitionally has no secantusAdmin.* commands.
    app.state.capabilities = _MONGODB
    r = await http.get("/maintenance", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/maintenance/prune-oplog"' not in r.text
    assert 'action="/maintenance/prune-ttl"' not in r.text
    assert "Prune oplog / TTL unavailable" in r.text
    # fsync is a standard command — stays available.
    assert 'action="/maintenance/fsync"' in r.text


async def test_maintenance_prune_shown_on_python(app, http: AsyncClient) -> None:
    app.state.capabilities = _PYTHON
    r = await http.get("/maintenance", headers=_AUTH)
    assert r.status_code == 200
    assert 'action="/maintenance/prune-oplog"' in r.text
    assert 'action="/maintenance/prune-ttl"' in r.text


async def test_lifespan_startup_probes_live_server(app) -> None:
    # The real startup probe (not just the pure classifier) must detect the
    # live embedded Python server and replace the UNKNOWN default.
    assert app.state.capabilities is capabilities.UNKNOWN
    async with app.router.lifespan_context(app):
        assert app.state.capabilities.kind == "python"
        assert app.state.capabilities.native_prune is True
