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
  * BSON type fidelity — ObjectId / int32 / int64 / double / Decimal128
    / Date / Binary round-trip through SecantusDB without type collapse.
  * Bulk write — mixed insert / update / replace / upsert / delete in
    one ``bulkWrite``; counts and final state match per-driver.
  * Change-stream resume — ``resumeAfter`` and
    ``startAtOperationTime`` round-trip; the resume token format is
    opaque to drivers, but each driver must re-present it verbatim
    and have the server replay the right next events.

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


# ---------------------------------------------------------------------------
# BSON type fidelity
# ---------------------------------------------------------------------------


# mongosh prints an extended-JSON representation per field; we parse it
# Python-side and assert each field's type tag (`$oid`, `$numberInt`,
# `$numberLong`, `$numberDouble`, `$numberDecimal`, `$date`, `$binary`).
# Anything missing the expected tag means SecantusDB collapsed the type
# at the wire — the exact bug class this smoke is built to catch.
_TYPES_MONGOSH_SCRIPT = """
const objID = new ObjectId();
const dec = NumberDecimal("3.141592653589793238");
const when = ISODate("2026-05-06T12:34:56.789Z");
const bin = BinData(0, "aGVsbG8=");
db.c.drop();
db.c.insertOne({
  _id: objID,
  i32: NumberInt(2147483647),
  i64: NumberLong("9223372036854775807"),
  f64: 2.5,
  dec: dec,
  dt: when,
  bin: bin,
  b: true,
  n: null,
});
const got = db.c.findOne({ _id: objID });
print(EJSON.stringify(got, { relaxed: false }));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_types_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run(
        [_MONGOSH, "--quiet", f"{server.uri}types_xd", "--eval", _TYPES_MONGOSH_SCRIPT],
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

    # Strict extJSON tags: collapsing at the server would drop these.
    assert "$oid" in payload["_id"], f"_id: {payload['_id']}"
    assert payload["i32"] == {"$numberInt": "2147483647"}
    assert payload["i64"] == {"$numberLong": "9223372036854775807"}
    assert payload["f64"] == {"$numberDouble": "2.5"}
    assert payload["dec"] == {"$numberDecimal": "3.141592653589793238"}
    assert payload["dt"] == {"$date": {"$numberLong": "1778070896789"}}
    assert payload["bin"]["$binary"]["subType"] == "00"
    assert payload["bin"]["$binary"]["base64"] == "aGVsbG8="
    assert payload["b"] is True
    assert payload["n"] is None


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_types_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "types_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node types smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_types_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./types"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go types smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Bulk write
# ---------------------------------------------------------------------------


# One mixed bulk: insert + updateOne + updateMany + replaceOne +
# upsert + deleteOne. Each driver folds the heterogeneous slice into
# one OP_MSG with a kind-1 documentSequence; this smoke proves the
# server's command dispatcher reconstructs the shape and runs the
# right per-op handler in each case. The mongosh script prints the
# result counters + the final doc list for Python-side assertion.
_BULK_MONGOSH_SCRIPT = """
db.c.drop();
db.c.insertMany([{ _id: 1, kind: "old" }, { _id: 2, kind: "old" }]);
const res = db.c.bulkWrite([
  { insertOne: { document: { _id: 3, kind: "fresh" } } },
  { updateOne: { filter: { _id: 1 }, update: { $set: { kind: "new" } } } },
  { updateMany: { filter: { kind: "old" }, update: { $set: { kind: "new" } } } },
  { replaceOne: { filter: { _id: 3 }, replacement: { _id: 3, kind: "replaced" } } },
  { updateOne: {
      filter: { _id: 99 },
      update: { $set: { kind: "upserted" } },
      upsert: true,
  } },
  { deleteOne: { filter: { _id: 2 } } },
]);
const docs = db.c.find({}).sort({ _id: 1 }).toArray();
print(JSON.stringify({
  insertedCount: res.insertedCount,
  matchedCount: res.matchedCount,
  modifiedCount: res.modifiedCount,
  upsertedCount: res.upsertedCount,
  deletedCount: res.deletedCount,
  docs: docs.map((d) => ({ _id: d._id, kind: d.kind })),
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_bulk_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run(
        [_MONGOSH, "--quiet", f"{server.uri}bulk_xd", "--eval", _BULK_MONGOSH_SCRIPT],
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
    assert payload["insertedCount"] == 1
    assert payload["matchedCount"] == 3
    assert payload["modifiedCount"] == 3
    assert payload["upsertedCount"] == 1
    assert payload["deletedCount"] == 1
    assert payload["docs"] == [
        {"_id": 1, "kind": "new"},
        {"_id": 3, "kind": "replaced"},
        {"_id": 99, "kind": "upserted"},
    ]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_bulk_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "bulk_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_bulk_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./bulk"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go bulk smoke: rc={result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# ---------------------------------------------------------------------------
# Change-stream resume
# ---------------------------------------------------------------------------


# Workload: open a watch, drive three inserts, capture the resume
# token at event 1, close, reopen with `resumeAfter` → must replay
# events 2 + 3 in order. Then reopen with `startAtOperationTime`
# anchored to a pre-insert timestamp → must replay all three.
#
# Resume tokens are opaque to drivers; the server-side layout is
# `{s, t, n, k}` BSON-encoded as a hex string (`secantus.changestreams`).
# Any wire-shape divergence in token round-trip surfaces here as a
# wrong starting position or a "resume token not found" error.
_CS_RESUME_MONGOSH_SCRIPT = """
db.c.drop();
const startTs = db.runCommand({ hello: 1 }).lastWrite.opTime.ts;

const cs1 = db.c.watch([], { maxAwaitTimeMS: 1000 });
sleep(200);
db.c.insertMany([{ _id: 1 }, { _id: 2 }, { _id: 3 }]);

function nextEvent(cs, deadline) {
  while (Date.now() < deadline) {
    if (cs.hasNext()) return cs.next();
    sleep(150);
  }
  return null;
}

const e1 = nextEvent(cs1, Date.now() + 8000);
const resumeAfter = e1._id;
cs1.close();

const cs2 = db.c.watch([], { resumeAfter, maxAwaitTimeMS: 1000 });
const e2 = nextEvent(cs2, Date.now() + 8000);
const e3 = nextEvent(cs2, Date.now() + 8000);
cs2.close();

const cs3 = db.c.watch([], { startAtOperationTime: startTs, maxAwaitTimeMS: 1000 });
const got = [];
const deadline = Date.now() + 8000;
while (got.length < 3 && Date.now() < deadline) {
  if (cs3.hasNext()) got.push(cs3.next().documentKey._id);
  else sleep(150);
}
cs3.close();

print(JSON.stringify({
  e1: e1.documentKey._id,
  e2: e2.documentKey._id,
  e3: e3.documentKey._id,
  startAt: got,
}));
"""


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_cs_resume_smoke_via_mongosh(server: SecantusDBServer) -> None:
    result = _run(
        [
            _MONGOSH,
            "--quiet",
            f"{server.uri}cs_resume_xd",
            "--eval",
            _CS_RESUME_MONGOSH_SCRIPT,
        ],
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
    assert payload["e1"] == 1
    assert payload["e2"] == 2
    assert payload["e3"] == 3
    assert payload["startAt"] == [1, 2, 3]


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_cs_resume_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "cs_resume_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node cs-resume smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_cs_resume_smoke_via_go_driver(server: SecantusDBServer) -> None:
    env = {**os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "./cs_resume"],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,
    )
    assert result.returncode == 0, (
        f"go cs-resume smoke: rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "OK" in result.stdout
