"""Cross-driver geo smoke tests.

The in-tree geo coverage (tests/test_geo_query.py + tests/test_geo_index.py)
drives ~50 cases through pymongo. Wire-protocol bugs that surface only
with a different driver's BSON serialization (Go, Node, mongosh's
node-driver) wouldn't be caught by those. This file plugs that gap with
a small canonical workload run through each driver:

  * insert three GeoJSON Points (origin, ~111 m east, far away)
  * create a `2dsphere` index
  * `$geoWithin` with `$centerSphere` — assert the close two come back
  * `$geoNear` aggregation — assert ordering + distances

Each test self-skips if its driver tooling isn't on PATH. Java is not
covered here because a single-file Java program can't pull in the
driver jar without Maven/Gradle scaffolding; the gap is documented in
``tasks/backlog.md``.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from secantus import SecantusDBServer

_HERE = Path(__file__).parent
_CROSS_DRIVER = _HERE / "cross_driver"


def _run(cmd: list[str], *, cwd: Path | None = None, env: dict[str, str] | None = None,
         timeout: float = 120.0) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
    )


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        yield srv


# --- mongosh ---------------------------------------------------------------


_MONGOSH = shutil.which("mongosh")


@pytest.mark.skipif(_MONGOSH is None, reason="mongosh not on PATH")
def test_geo_smoke_via_mongosh(server: SecantusDBServer) -> None:
    """Geo round-trip via mongosh (which uses the node-driver underneath).

    mongosh's `--eval` runs the script then exits; we wrap the workload
    in JSON.stringify so the result lands on stdout in a parseable form.
    Doc IDs are compared as a set (Mongo doesn't guarantee `$geoWithin`
    order); `$geoNear` results are compared in ascending-distance order.
    """
    script = """
    db.places.drop();
    db.places.insertMany([
      { _id: 1, loc: { type: "Point", coordinates: [0.0, 0.0] } },
      { _id: 2, loc: { type: "Point", coordinates: [0.001, 0.0] } },
      { _id: 3, loc: { type: "Point", coordinates: [50.0, 50.0] } },
    ]);
    db.places.createIndex({ loc: "2dsphere" });

    const within = db.places
      .find({ loc: { $geoWithin: { $centerSphere: [[0, 0], 0.001] } } }, { _id: 1 })
      .toArray();

    const near = db.places
      .aggregate([
        {
          $geoNear: {
            near: { type: "Point", coordinates: [0, 0] },
            distanceField: "d",
            key: "loc",
            maxDistance: 200,
          },
        },
      ])
      .toArray();

    print(JSON.stringify({
      withinIds: within.map((d) => d._id).sort(),
      nearIds: near.map((d) => d._id),
      d0: near[0].d,
      d1: near[1].d,
    }));
    """
    result = _run(
        [_MONGOSH, "--quiet", f"{server.uri}geo_xdriver", "--eval", script],
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"mongosh exited {result.returncode}\nstdout: {result.stdout}\n"
        f"stderr: {result.stderr}"
    )

    # mongosh prefixes connection-status lines before our JSON.stringify line;
    # find the JSON object in the output.
    out_line = next(
        (ln for ln in reversed(result.stdout.splitlines()) if ln.startswith("{")),
        None,
    )
    assert out_line is not None, f"no JSON line in mongosh output: {result.stdout!r}"
    payload = json.loads(out_line)
    assert payload["withinIds"] == [1, 2]
    assert payload["nearIds"] == [1, 2]
    assert payload["d0"] == pytest.approx(0.0, abs=1e-6)
    assert 100 < payload["d1"] < 130


# --- Node (mongo-node-driver) ---------------------------------------------


_NODE = shutil.which("node")
_NPM = shutil.which("npm")
_NODE_SMOKE_DIR = _CROSS_DRIVER / "node"


def _ensure_node_modules() -> bool:
    """Install mongodb npm package once; cached in the dir for re-runs.

    Returns False if `npm install` fails (e.g. offline). The pytest
    skipif then passes the failure reason through.
    """
    nm = _NODE_SMOKE_DIR / "node_modules"
    if nm.is_dir():
        return True
    if _NPM is None:
        return False
    result = _run([_NPM, "install", "--silent"], cwd=_NODE_SMOKE_DIR, timeout=300.0)
    return result.returncode == 0 and nm.is_dir()


@pytest.mark.skipif(_NODE is None, reason="node not on PATH")
@pytest.mark.skipif(_NPM is None, reason="npm not on PATH")
def test_geo_smoke_via_node_driver(server: SecantusDBServer) -> None:
    if not _ensure_node_modules():
        pytest.skip("could not install mongodb npm package")
    env = {"MONGODB_URI": server.uri, "PATH": sys.executable.rsplit("/", 1)[0]}
    # subprocess.run inherits PATH from os.environ if not specified;
    # we want the real PATH so node can find its own modules.
    import os as _os

    env = {**_os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_NODE, str(_NODE_SMOKE_DIR / "geo_smoke.js")],
        env=env,
        timeout=60.0,
    )
    assert result.returncode == 0, (
        f"node smoke exited {result.returncode}\nstdout: {result.stdout}\n"
        f"stderr: {result.stderr}"
    )
    assert "OK" in result.stdout


# --- Go (mongo-go-driver) -------------------------------------------------


_GO = shutil.which("go")
_GO_SMOKE_DIR = _CROSS_DRIVER / "go"


@pytest.mark.skipif(_GO is None, reason="go not on PATH")
def test_geo_smoke_via_go_driver(server: SecantusDBServer) -> None:
    """First run downloads mongo-go-driver into the local module cache;
    subsequent runs use the cached download (a few hundred ms)."""
    import os as _os

    env = {**_os.environ, "MONGODB_URI": server.uri}
    result = _run(
        [_GO, "run", "."],
        cwd=_GO_SMOKE_DIR,
        env=env,
        timeout=180.0,  # first run pulls the driver
    )
    assert result.returncode == 0, (
        f"go smoke exited {result.returncode}\nstdout: {result.stdout}\n"
        f"stderr: {result.stderr}"
    )
    assert "OK" in result.stdout
