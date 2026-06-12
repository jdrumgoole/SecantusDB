"""End-to-end mongoimport / mongoexport against an embedded SecantusDB.

Both tools are built on mongo-go-driver, so like mongodump/mongorestore
they hard-fail on wire-protocol type sloppiness that pymongo silently
tolerates. mongoexport exercises find with query/sort/fields; mongoimport
exercises batched inserts and drop-on-import.

The tests skip gracefully if the tools aren't on PATH so they don't
break local runs without the MongoDB Database Tools installed (CI image
must install them explicitly).
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

MONGOIMPORT = shutil.which("mongoimport")
MONGOEXPORT = shutil.which("mongoexport")
HAVE_TOOLS = MONGOIMPORT is not None and MONGOEXPORT is not None

pytestmark = pytest.mark.skipif(
    not HAVE_TOOLS,
    reason="mongoimport / mongoexport not on PATH (install MongoDB Database Tools)",
)


def test_export_round_trip(tmp_path: Path) -> None:
    """pymongo writes → mongoexport reads back the same docs as NDJSON."""
    docs = [
        {"_id": 1, "name": "Pommard 2018", "year": 2018},
        {"_id": 2, "name": "Brunello 2015", "year": 2015},
        {"_id": 3, "name": "Barolo 2017", "year": 2017},
    ]

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["wine_cellar"]["bottles"].insert_many(docs)

            out_file = tmp_path / "bottles.json"
            assert MONGOEXPORT is not None  # narrowed by skipif
            subprocess.run(
                [
                    MONGOEXPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "wine_cellar",
                    "--collection",
                    "bottles",
                    "--sort",
                    '{"_id": 1}',
                    "--out",
                    str(out_file),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            exported = [
                json.loads(line) for line in out_file.read_text().splitlines() if line.strip()
            ]
            assert exported == docs
        finally:
            client.close()


def test_export_honours_query_and_fields(tmp_path: Path) -> None:
    """mongoexport --query/--fields drive find's filter + projection."""
    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["wine_cellar"]["bottles"].insert_many(
                [{"_id": i, "name": f"bottle-{i}", "year": 2010 + i} for i in range(6)]
            )

            out_file = tmp_path / "recent.json"
            assert MONGOEXPORT is not None
            subprocess.run(
                [
                    MONGOEXPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "wine_cellar",
                    "--collection",
                    "bottles",
                    "--query",
                    '{"year": {"$gte": 2013}}',
                    "--fields",
                    "year",
                    "--sort",
                    '{"_id": 1}',
                    "--out",
                    str(out_file),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            exported = [
                json.loads(line) for line in out_file.read_text().splitlines() if line.strip()
            ]
            assert exported == [
                {"_id": 3, "year": 2013},
                {"_id": 4, "year": 2014},
                {"_id": 5, "year": 2015},
            ]
        finally:
            client.close()


def test_import_round_trip(tmp_path: Path) -> None:
    """mongoimport writes NDJSON → pymongo reads the same docs back."""
    docs = [{"_id": i, "tag": f"t{i}", "qty": i * 2} for i in range(5)]
    ndjson = tmp_path / "import.json"
    ndjson.write_text("\n".join(json.dumps(d) for d in docs))

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            assert MONGOIMPORT is not None  # narrowed by skipif
            subprocess.run(
                [
                    MONGOIMPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "shop",
                    "--collection",
                    "imported",
                    "--file",
                    str(ndjson),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            assert list(client["shop"]["imported"].find().sort("_id")) == docs
        finally:
            client.close()


def test_import_drop_replaces_existing(tmp_path: Path) -> None:
    """mongoimport --drop wipes the target collection before loading."""
    ndjson = tmp_path / "import.json"
    ndjson.write_text(json.dumps({"_id": 100, "fresh": True}))

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            stale = client["shop"]["imported"]
            stale.insert_many([{"_id": i, "stale": True} for i in range(3)])

            assert MONGOIMPORT is not None
            subprocess.run(
                [
                    MONGOIMPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "shop",
                    "--collection",
                    "imported",
                    "--drop",
                    "--file",
                    str(ndjson),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            assert list(stale.find()) == [{"_id": 100, "fresh": True}]
        finally:
            client.close()


def test_export_csv(tmp_path: Path) -> None:
    """mongoexport --type=csv emits a header row plus one line per doc."""
    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["wine_cellar"]["bottles"].insert_many(
                [
                    {"_id": 1, "name": "Pommard", "year": 2018},
                    {"_id": 2, "name": "Barolo", "year": 2017},
                ]
            )

            out_file = tmp_path / "bottles.csv"
            assert MONGOEXPORT is not None
            subprocess.run(
                [
                    MONGOEXPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "wine_cellar",
                    "--collection",
                    "bottles",
                    "--type",
                    "csv",
                    "--fields",
                    "name,year",
                    "--sort",
                    '{"_id": 1}',
                    "--out",
                    str(out_file),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            assert out_file.read_text().splitlines() == [
                "name,year",
                "Pommard,2018",
                "Barolo,2017",
            ]
        finally:
            client.close()


def test_round_trip_preserves_bson_types(tmp_path: Path) -> None:
    """Export → import through canonical extended JSON keeps ObjectId,
    datetime, Decimal128, Int64, and Binary intact — the type-fidelity
    contract that makes mongoexport/mongoimport a real backup path."""
    import datetime

    from bson import Binary, Decimal128, Int64, ObjectId

    doc = {
        "_id": ObjectId(),
        "when": datetime.datetime(2026, 6, 12, 10, 30, tzinfo=datetime.timezone.utc),
        "price": Decimal128("19.99"),
        "big": Int64(2**40),
        "blob": Binary(b"\x00\x01\x02"),
    }

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["shop"]["typed"].insert_one(doc)

            out_file = tmp_path / "typed.json"
            assert MONGOEXPORT is not None
            subprocess.run(
                [
                    MONGOEXPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "shop",
                    "--collection",
                    "typed",
                    "--jsonFormat",
                    "canonical",
                    "--out",
                    str(out_file),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            assert MONGOIMPORT is not None
            subprocess.run(
                [
                    MONGOIMPORT,
                    "--uri",
                    server.uri,
                    "--db",
                    "shop",
                    "--collection",
                    "reimported",
                    "--file",
                    str(out_file),
                ],
                check=True,
                capture_output=True,
                timeout=30,
            )

            # Compare what the server stored, read back through the same
            # client — sidesteps pymongo's client-side decode defaults
            # (naive datetimes, subtype-0 Binary -> bytes) symmetrically.
            original = client["shop"]["typed"].find_one()
            reimported = client["shop"]["reimported"].find_one()
            assert original is not None and reimported is not None
            assert reimported == original
            assert reimported["_id"] == doc["_id"]
            assert reimported["price"] == doc["price"]
            assert reimported["big"] == doc["big"]
        finally:
            client.close()
