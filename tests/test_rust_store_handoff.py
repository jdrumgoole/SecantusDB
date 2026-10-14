"""A store handed between the Rust and the Python MongoDB servers.

The two servers share one on-disk format, but they key a COLLATED index and a
GEO index differently (ICU4X sort keys against folded strings; the Rust
server's geo rules since 2026-10-13), and only the Rust server claims a
stored ``_id`` under its collection's collation. Each server therefore marks
the collated and geo indexes it builds, and rebuilds one without its own
marker when it opens a store; the Python server also removes the Rust
server's record that every ``_id`` is claimed, so the Rust server claims them
again.

Each test hands one store Rust -> Python -> Rust, and checks after each hand
that the server holding it answers from what the other one wrote. Before these
rules, every check marked below as "the other server's entries" was a silent
wrong answer.

Gated on the WiredTiger-linking ``_secantus_server`` extension.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from pymongo.errors import DuplicateKeyError  # noqa: E402

from secantus import SecantusDBServer  # noqa: E402

CI = {"locale": "en", "strength": 2}


class _Rust:
    def __init__(self, path: Path) -> None:
        self.srv = _server.RustServer(str(path), 0)

    def __enter__(self) -> Any:
        host, port = self.srv.address
        return pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
        )["app"]

    def __exit__(self, *exc: object) -> None:
        self.srv.stop()


class _Python:
    def __init__(self, path: Path) -> None:
        self.srv = SecantusDBServer(port=0, storage_path=str(path))

    def __enter__(self) -> Any:
        self.srv.__enter__()
        return pymongo.MongoClient(self.srv.uri, directConnection=True)["app"]

    def __exit__(self, *exc: object) -> None:
        self.srv.__exit__(None, None, None)


def _index(db: Any, coll: str, name: str) -> dict[str, Any]:
    return next(ix for ix in db[coll].list_indexes() if ix["name"] == name)


def test_a_unique_collated_index_holds_across_both_servers(tmp_path: Path) -> None:
    data = tmp_path / "store"
    with _Rust(data) as db:
        db.c.create_index("s", name="s_ci", unique=True, collation=CI)
        db.c.insert_one({"_id": 1, "s": "a"})
    with _Python(data) as db:
        # The Rust server's entries.
        with pytest.raises(DuplicateKeyError):
            db.c.insert_one({"_id": 2, "s": "A"})
        db.c.insert_one({"_id": 3, "s": "b"})
        # Neither server's marker reaches a client.
        assert set(_index(db, "c", "s_ci")) == {"v", "key", "name", "unique", "collation"}
    with _Rust(data) as db:
        # The Python server's entries.
        with pytest.raises(DuplicateKeyError):
            db.c.insert_one({"_id": 4, "s": "B"})
        with pytest.raises(DuplicateKeyError):
            db.c.insert_one({"_id": 5, "s": "A"})
        assert set(_index(db, "c", "s_ci")) == {"v", "key", "name", "unique", "collation"}
        assert sorted(d["_id"] for d in db.c.find()) == [1, 3]


def test_collated_ids_the_python_server_stored_are_claimed(tmp_path: Path) -> None:
    data = tmp_path / "store"
    with _Rust(data) as db:
        db.create_collection("k", collation=CI)
        db.k.insert_one({"_id": "a"})
    with _Python(data) as db:
        db.k.insert_one({"_id": "b"})
    with _Rust(data) as db:
        for spelling in ("A", "B"):
            with pytest.raises(DuplicateKeyError):
                db.k.insert_one({"_id": spelling})
        assert sorted(d["_id"] for d in db.k.find()) == ["a", "b"]


def test_a_geo_index_finds_what_either_server_wrote(tmp_path: Path) -> None:
    data = tmp_path / "store"
    box = {"loc": {"$geoWithin": {"$box": [[0, 0], [10, 10]]}}}
    with _Rust(data) as db:
        db.g.create_index([("loc", "2d")], name="geo")
        db.g.insert_one({"_id": 1, "loc": [1, 1]})
    with _Python(data) as db:
        db.g.insert_one({"_id": 2, "loc": [2, 2]})
        assert sorted(d["_id"] for d in db.g.find(box)) == [1, 2]
        assert set(_index(db, "g", "geo")) == {"v", "key", "name"}
    with _Rust(data) as db:
        db.g.insert_one({"_id": 3, "loc": [[3, 3], [40, 40]]})
        assert sorted(d["_id"] for d in db.g.find(box)) == [1, 2, 3]
        assert set(_index(db, "g", "geo")) == {"v", "key", "name"}
