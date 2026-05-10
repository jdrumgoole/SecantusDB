"""End-to-end mongosh smoke against an embedded SecantusDB.

mongosh is the official MongoDB shell — Node.js-based, built on
mongo-node-driver. Like mongodump/mongorestore (Go-driver-based),
it's a real third-party client that exercises wire-protocol
compatibility independent of pymongo's permissiveness. Round-tripping
through it proves the surface real users hit isn't quietly broken.

The test skips gracefully if `mongosh` isn't on PATH so it doesn't
break local runs without it (CI image must install it explicitly).

Two-direction round-trip:
  - mongosh writes, pymongo reads.
  - pymongo writes, mongosh reads (output parsed from JSON).
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

MONGOSH = shutil.which("mongosh")

# Windows runners ship mongosh on PATH but the ``--eval`` JSON
# argument round-trips through PowerShell's quoting, which mangles
# the embedded ``"`` characters and the subprocess hangs to timeout.
# Linux + macOS runners cover the wire-protocol round-trip these
# tests are meant to catch.
pytestmark = pytest.mark.skipif(
    MONGOSH is None or sys.platform == "win32",
    reason="mongosh not on PATH (install MongoDB Shell) or running on "
    "Windows (subprocess --eval argument quoting hangs).",
)


def _run_mongosh(uri: str, eval_script: str) -> str:
    """Run a one-shot mongosh script and return stdout (stripped).

    On non-zero exit, the assertion message includes both stdout and
    stderr so xdist test failures stay diagnosable — the default
    ``CalledProcessError`` repr drops both streams.
    """
    assert MONGOSH is not None  # narrowed by skipif
    result = subprocess.run(
        [MONGOSH, uri, "--quiet", "--eval", eval_script],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"mongosh exited with code {result.returncode}\n"
            f"--- script ---\n{eval_script}\n"
            f"--- stdout ---\n{result.stdout}\n"
            f"--- stderr ---\n{result.stderr}"
        )
    return result.stdout.strip()


def test_mongosh_writes_pymongo_reads(tmp_path) -> None:
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as server:
        # mongosh inserts.
        out = _run_mongosh(
            f"{server.uri}wine_cellar",
            'JSON.stringify(db.bottles.insertOne({_id: 1, name: "Pommard 2018", year: 2018}))',
        )
        # mongosh's JSON.stringify wraps Long/ObjectId types specially; we
        # only need confirmation the insert acknowledged.
        assert "acknowledged" in out

        # pymongo reads back.
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            doc = client["wine_cellar"]["bottles"].find_one({"_id": 1})
            assert doc == {"_id": 1, "name": "Pommard 2018", "year": 2018}
        finally:
            client.close()


def test_pymongo_writes_mongosh_reads(tmp_path) -> None:
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as server:
        # pymongo inserts.
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["wine_cellar"]["bottles"].insert_many(
                [
                    {"_id": 1, "name": "Pommard 2018", "year": 2018},
                    {"_id": 2, "name": "Brunello 2015", "year": 2015},
                ]
            )
        finally:
            client.close()

        # mongosh reads back via JSON.stringify.
        out = _run_mongosh(
            f"{server.uri}wine_cellar",
            "JSON.stringify(db.bottles.find({}).sort({_id: 1}).toArray())",
        )
        try:
            docs = json.loads(out)
        except json.JSONDecodeError as e:
            raise AssertionError(
                f"could not parse mongosh stdout as JSON: {e}\n--- stdout ---\n{out}"
            ) from e
        assert docs == [
            {"_id": 1, "name": "Pommard 2018", "year": 2018},
            {"_id": 2, "name": "Brunello 2015", "year": 2015},
        ]


def test_mongosh_index_round_trip(tmp_path) -> None:
    """mongosh's createIndex + listIndexes round-trip through SecantusDB."""
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as server:
        # Split into two mongosh invocations: the first does the writes
        # (whose return values mongosh would otherwise auto-print and
        # complicate parsing); the second runs the single read whose
        # JSON.stringify result is the only line of stdout we then parse.
        # Earlier single-invocation form scanned for the last line
        # starting with '[' and was occasionally fragile under heavy
        # parallel load.
        _run_mongosh(
            f"{server.uri}wine_cellar",
            "db.bottles.insertOne({_id: 1, year: 2018}); db.bottles.createIndex({year: 1});",
        )
        out = _run_mongosh(
            f"{server.uri}wine_cellar",
            "JSON.stringify(db.bottles.getIndexes().map(i => i.name))",
        )
        try:
            names = json.loads(out)
        except json.JSONDecodeError as e:
            raise AssertionError(
                f"could not parse mongosh stdout as JSON: {e}\n--- stdout ---\n{out}"
            ) from e
        assert "_id_" in names, f"_id_ missing from {names}"
        assert "year_1" in names, f"year_1 missing from {names}"
