"""Cross-driver smoke tests for recently-added features.

The in-tree pytest suite drives these features through pymongo only.
Wire-protocol bugs that surface only with a different driver's BSON
serialization, command shape, or error mapping wouldn't be caught.
This file plugs that gap with one canonical workload per feature, run
through each driver:

  * RBAC denial — `read`-bound user, find succeeds, insert rejected
    with code 13 / Unauthorized.
  * DDL change events — createIndex + dropIndex emit
    ``operationType: createIndexes`` / ``dropIndexes``.
  * ``updateUser`` rotation — old password rejected, new password
    accepted.
  * ``connectionStatus`` — authenticated user roles surfaced via
    ``authInfo.authenticatedUserRoles``.

Each test self-skips if its driver tooling isn't on PATH. Java is not
covered here for the same reason as the geo smoke tests: a
single-file Java program can't pull in the driver jar without
Maven/Gradle scaffolding.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

from secantus import SecantusDBServer
from secantus.auth import SCRAM_SHA_256, derive_credentials

_HERE = Path(__file__).parent
_CROSS_DRIVER = _HERE / "cross_driver"
_NODE_SMOKE_DIR = _CROSS_DRIVER / "node"
_GO_SMOKE_DIR = _CROSS_DRIVER / "go"

_MONGOSH = shutil.which("mongosh")
_NODE = shutil.which("node")
_NPM = shutil.which("npm")
_GO = shutil.which("go")


def _run(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 120.0,
) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)


def _ensure_node_modules() -> bool:
    """Install mongodb npm package once; cached in dir for re-runs."""
    nm = _NODE_SMOKE_DIR / "node_modules"
    if nm.is_dir():
        return True
    if _NPM is None:
        return False
    result = _run([_NPM, "install", "--silent"], cwd=_NODE_SMOKE_DIR, timeout=300.0)
    return result.returncode == 0 and nm.is_dir()


# ---------------------------------------------------------------------------
# Server fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def server(tmp_path):
    """Plain (no-auth) server — used by DDL smoke."""
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        yield srv


_ADMIN_USER = "root"
_ADMIN_PWD = "rootpw"


@pytest.fixture
def server_with_auth(tmp_path):
    """Auth-enabled server with a bootstrap root/`rootpw` admin user."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "wt"), require_auth=True)
    srv.start()
    creds = derive_credentials(_ADMIN_PWD)
    record = {
        "_id": "admin.root",
        "user": _ADMIN_USER,
        "db": "admin",
        "credentials": creds.to_doc(),
        "roles": [{"role": "root", "db": "admin"}],
        "mechanisms": [SCRAM_SHA_256],
    }
    srv.storage.add_user("admin", _ADMIN_USER, record)
    try:
        yield srv
    finally:
        srv.stop()


# ---------------------------------------------------------------------------
# RBAC denial
# ---------------------------------------------------------------------------


_RBAC_MONGOSH_SCRIPT = """
db.getSiblingDB("shop").runCommand({
  createUser: "viewer",
  pwd: "vp",
  roles: [{ role: "read", db: "shop" }],
});
print("PROVISIONED");
"""

_RBAC_MONGOSH_VIEWER_SCRIPT = """
const items = db.getSiblingDB("shop").items;
const findRes = items.find({}).toArray();
let inserted = false;
let errCode = null;
let errMsg = null;
try {
  items.insertOne({ x: 1 });
  inserted = true;
} catch (e) {
  errCode = e.code;
  errMsg = e.errmsg || e.message || String(e);
}
print(JSON.stringify({ findCount: findRes.length, inserted, errCode, errMsg }));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_rbac_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    """RBAC denial round-trip via mongosh.

    First mongosh invocation provisions the viewer with admin creds;
    second invocation re-connects as the viewer and asserts find works
    while insert is rejected.
    """
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run([_MONGOSH, "--quiet", admin_uri, "--eval", _RBAC_MONGOSH_SCRIPT], timeout=60.0)
    assert result.returncode == 0 and "PROVISIONED" in result.stdout, (
        f"provision: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    viewer_uri = (
        f"mongodb://viewer:vp@127.0.0.1:{server_with_auth.port}/shop"
        "?authSource=shop&authMechanism=SCRAM-SHA-256"
    )
    result = _run(
        [_MONGOSH, "--quiet", viewer_uri, "--eval", _RBAC_MONGOSH_VIEWER_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"viewer: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["inserted"] is False
    assert payload["errCode"] == 13 or "Unauthorized" in (payload["errMsg"] or "")


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_rbac_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "rbac_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_rbac_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./rbac"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go rbac smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# DDL change events
# ---------------------------------------------------------------------------


_DDL_MONGOSH_SCRIPT = """
const coll = db.getSiblingDB("ddl_xd").c;
coll.drop();
coll.insertOne({ _id: 1 });

const cs = coll.watch([], { maxAwaitTimeMS: 2000 });
sleep(300);

coll.createIndex({ x: 1 });
coll.dropIndex("x_1");

const events = [];
const deadline = Date.now() + 8000;
while (Date.now() < deadline && events.length < 2) {
  if (cs.hasNext()) {
    events.push(cs.next().operationType);
  } else {
    sleep(200);
  }
}
cs.close();
print(JSON.stringify({ events }));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_ddl_smoke_via_mongosh(server: SecantusDBServer) -> None:
    """DDL change-stream events via mongosh."""
    result = _run(
        [_MONGOSH, "--quiet", f"{server.uri}ddl_xd", "--eval", _DDL_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["events"] == ["createIndexes", "dropIndexes"]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_ddl_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "ddl_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node ddl smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_ddl_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./ddl"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go ddl smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# updateUser rotation
# ---------------------------------------------------------------------------


_UPDATEUSER_MONGOSH_PROVISION_SCRIPT = """
db.getSiblingDB("admin").runCommand({
  createUser: "alice_xd",
  pwd: "orig",
  roles: [{ role: "read", db: "admin" }],
});
db.getSiblingDB("admin").runCommand({ updateUser: "alice_xd", pwd: "rotated" });
print("PROVISIONED");
"""

_UPDATEUSER_MONGOSH_PING_SCRIPT = (
    'print(JSON.stringify({ ok: db.getSiblingDB("admin").runCommand({ ping: 1 }).ok }));'
)


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_updateuser_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    """updateUser rotation via mongosh."""
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _UPDATEUSER_MONGOSH_PROVISION_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0 and "PROVISIONED" in result.stdout, (
        f"provision: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # Old password — must fail.
    old_uri = (
        f"mongodb://alice_xd:orig@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run(
        [_MONGOSH, "--quiet", old_uri, "--eval", _UPDATEUSER_MONGOSH_PING_SCRIPT],
        timeout=30.0,
    )
    assert result.returncode != 0 or '"ok":1' not in result.stdout, (
        f"old password should not authenticate: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # New password — must succeed.
    new_uri = (
        f"mongodb://alice_xd:rotated@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run(
        [_MONGOSH, "--quiet", new_uri, "--eval", _UPDATEUSER_MONGOSH_PING_SCRIPT],
        timeout=30.0,
    )
    assert result.returncode == 0, (
        f"new password: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["ok"] == 1


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_updateuser_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "updateuser_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node updateuser smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_updateuser_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./updateuser"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go updateuser smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# connectionStatus / authenticatedUserRoles
# ---------------------------------------------------------------------------


_CONNSTATUS_MONGOSH_SCRIPT = """
const r = db.getSiblingDB("admin").runCommand({ connectionStatus: 1 });
print(JSON.stringify({
  users: r.authInfo.authenticatedUsers,
  roles: r.authInfo.authenticatedUserRoles,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_connstatus_smoke_via_mongosh(server_with_auth: SecantusDBServer) -> None:
    """connectionStatus surfaces authenticatedUserRoles via mongosh."""
    admin_uri = (
        f"mongodb://{_ADMIN_USER}:{_ADMIN_PWD}@127.0.0.1:{server_with_auth.port}/"
        "?authSource=admin&authMechanism=SCRAM-SHA-256"
    )
    result = _run(
        [_MONGOSH, "--quiet", admin_uri, "--eval", _CONNSTATUS_MONGOSH_SCRIPT],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["users"] and payload["users"][0]["user"] == _ADMIN_USER
    assert payload["roles"] and payload["roles"][0]["role"] == "root"


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_connstatus_smoke_via_node_driver(server_with_auth: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "connstatus_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node connstatus smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_connstatus_smoke_via_go_driver(server_with_auth: SecantusDBServer) -> None:
    env = {
        **os.environ,
        "MONGODB_URI": server_with_auth.uri,
        "ADMIN_PASSWORD": _ADMIN_PWD,
    }
    result = _run(
        [_GO, "run", "./connstatus"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go connstatus smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout
