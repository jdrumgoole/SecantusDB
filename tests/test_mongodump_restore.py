"""End-to-end mongodump / mongorestore against an embedded SecantusDB.

Regression cover for the wire-protocol int64 bugs in `_hello`
(topologyVersion.counter, connectionId) and every cursor reply
(`cursor.id`). The pymongo conformance suite missed both because
pymongo accepts int32 silently; the Go driver (mongo-go-driver,
which mongodump and mongorestore are built on) hard-fails on type
mismatch.

The test skips gracefully if `mongodump` / `mongorestore` aren't on
PATH so it doesn't break local runs that don't have the MongoDB
Database Tools installed (CI image must install them explicitly).
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

MONGODUMP = shutil.which("mongodump")
MONGORESTORE = shutil.which("mongorestore")
HAVE_TOOLS = MONGODUMP is not None and MONGORESTORE is not None

pytestmark = pytest.mark.skipif(
    not HAVE_TOOLS,
    reason="mongodump / mongorestore not on PATH (install MongoDB Database Tools)",
)


def test_dump_restore_round_trip(tmp_path: Path) -> None:
    docs = [
        {"_id": 1, "name": "Pommard 2018", "year": 2018},
        {"_id": 2, "name": "Brunello 2015", "year": 2015},
        {"_id": 3, "name": "Barolo 2017", "year": 2017},
    ]

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        uri = server.uri
        client = MongoClient(uri, serverSelectionTimeoutMS=2000)
        try:
            bottles = client["wine_cellar"]["bottles"]
            bottles.insert_many(docs)
            bottles.create_index([("year", 1)])

            dump_dir = tmp_path / "dump"
            assert MONGODUMP is not None  # narrowed by skipif
            subprocess.run(
                [MONGODUMP, "--uri", uri, "--db", "wine_cellar", "-o", str(dump_dir)],
                check=True,
                capture_output=True,
                timeout=30,
            )
            # Confirm dump produced the expected layout.
            assert (dump_dir / "wine_cellar" / "bottles.bson").is_file()
            assert (dump_dir / "wine_cellar" / "bottles.metadata.json").is_file()

            # Wipe and restore.
            client.drop_database("wine_cellar")
            assert "wine_cellar" not in client.list_database_names()

            assert MONGORESTORE is not None
            subprocess.run(
                [MONGORESTORE, "--uri", uri, str(dump_dir)],
                check=True,
                capture_output=True,
                timeout=30,
            )

            # Verify documents AND the user-defined index came back.
            restored = list(bottles.find().sort("_id"))
            assert restored == docs
            index_names = {ix["name"] for ix in bottles.list_indexes()}
            assert index_names == {"_id_", "year_1"}
        finally:
            client.close()


BSONDUMP = shutil.which("bsondump")


@pytest.mark.skipif(BSONDUMP is None, reason="bsondump not on PATH")
def test_bsondump_decodes_dump_output(tmp_path: Path) -> None:
    """bsondump round-trips the .bson mongodump produced — pins the dump
    file format itself, independent of mongorestore."""
    import json

    docs = [{"_id": i, "n": i * 10} for i in range(4)]

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["shop"]["items"].insert_many(docs)
            dump_dir = tmp_path / "dump"
            assert MONGODUMP is not None
            subprocess.run(
                [MONGODUMP, "--uri", server.uri, "--db", "shop", "-o", str(dump_dir)],
                check=True,
                capture_output=True,
                timeout=30,
            )
        finally:
            client.close()

    assert BSONDUMP is not None  # narrowed by skipif
    result = subprocess.run(
        [BSONDUMP, "--type", "json", str(tmp_path / "dump" / "shop" / "items.bson")],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    # bsondump emits canonical extended JSON, one doc per line.
    decoded = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
    assert decoded == [
        {"_id": {"$numberInt": str(i)}, "n": {"$numberInt": str(i * 10)}} for i in range(4)
    ]
