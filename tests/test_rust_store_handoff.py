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
# The Python server writes without timestamps, which a store whose data
# tables are not logged (secantusd-rs's default since RM-210) refuses once a
# key has one: the hand-off is tested on a store created with them logged.
pymongo = pytest.importorskip("pymongo")

from pymongo.errors import DuplicateKeyError  # noqa: E402

from secantus import SecantusDBServer  # noqa: E402

CI = {"locale": "en", "strength": 2}


class _Rust:
    def __init__(self, path: Path) -> None:
        self.srv = _server.RustServer(str(path), 0, data_nonlogged=False)

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


def _python_listed(db: Any, coll: str, name: str) -> set[str]:
    """The option names the deprecated Python server lists for an index,
    less `buildSeq` and `multikeyPaths`: the Rust server's build-order stamp
    (RM-081) and recorded multikey paths (RM-055), which the
    Python server does not know to hide. Compatibility with the Python
    server no longer binds (CLAUDE.md, 2026-10-16), so this test asserts
    what holds across the hand-off -- each server's answers -- and leaves
    that one key out rather than teaching the Python server about it."""
    return set(_index(db, coll, name)) - {"buildSeq", "multikeyPaths"}


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
        assert _python_listed(db, "c", "s_ci") == {"v", "key", "name", "unique", "collation"}
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
        assert _python_listed(db, "g", "geo") == {"v", "key", "name"}
    with _Rust(data) as db:
        db.g.insert_one({"_id": 3, "loc": [[3, 3], [40, 40]]})
        assert sorted(d["_id"] for d in db.g.find(box)) == [1, 2, 3]
        assert set(_index(db, "g", "geo")) == {"v", "key", "name"}


def test_number_keys_are_read_alike_by_both_servers(tmp_path: Path) -> None:
    """Both servers key a number by its exact value with a two-byte exponent
    (number-key format 2), so each finds the other's `_id`s and index entries
    -- a double `0.1` beside `Decimal128("0.1")`, numbers beyond exponent
    +-127 -- and neither rebuilds the other's indexes."""
    from bson.decimal128 import Decimal128

    data = tmp_path / "store"
    with _Rust(data) as db:
        db.n.create_index("a", name="a_1")
        db.n.insert_many(
            [
                {"_id": 0.1, "a": 1e-300},
                {"_id": Decimal128("0.1"), "a": 3e250},
                {"_id": 1e-300, "a": -1e-300},
            ]
        )
    with _Python(data) as db:
        # The Rust server's `_id` keys and entries.
        assert db.n.find_one({"_id": Decimal128("0.1")})["a"] == 3e250
        assert db.n.find_one({"_id": 1e-300})["a"] == -1e-300
        assert sorted(d["a"] for d in db.n.find({"a": {"$gt": 1e200}})) == [3e250]
        with pytest.raises(DuplicateKeyError):
            db.n.insert_one({"_id": 1e-300})
        db.n.insert_one({"_id": 3e250, "a": 2e-300})
        assert _python_listed(db, "n", "a_1") == {"v", "key", "name"}
    with _Rust(data) as db:
        # The Python server's.
        assert db.n.find_one({"_id": 3e250})["a"] == 2e-300
        assert sorted(d["a"] for d in db.n.find({"a": {"$lt": 1}})) == [-1e-300, 1e-300, 2e-300]
        with pytest.raises(DuplicateKeyError):
            db.n.insert_one({"_id": 3e250})
        assert [d["_id"] for d in db.n.find({"_id": {"$lt": 1}}).sort("_id", 1)] == [
            1e-300,
            Decimal128("0.1"),
            0.1,
        ]
