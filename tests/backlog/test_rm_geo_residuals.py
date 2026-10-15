"""Gap tests for what was left open after RM-042, RM-043, RM-045 and RM-047
(section 7.081 of `tasks/backlog-rust-mongodb.md`): a position S2 settles
by exact arithmetic, the `2dsphere` cell-level options, and a refused
`createIndexes` that leaves its collection behind. Every answer is mongod
8.2.11's, measured 2026-10-15 on a one-member replica set and on a
standalone (they agree).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

pymongo = pytest.importorskip("pymongo")


def _fresh(client: Any, name: str) -> Any:
    client.drop_database(name)
    return client[name]


@pytest.mark.backlog("RM-148")
def test_line_ending_exactly_on_another_line(rust_mongo: Any) -> None:
    """mongod 8.2.11: a line whose end lies exactly on another line's edge
    (the meridian at 11) does not intersect it. S2 settles a point exactly
    on a great circle by exact arithmetic and symbolic perturbation; this
    server's floating-point test counts it as touching."""
    db = _fresh(rust_mongo, "rmgeo_t")
    db.c.insert_one(
        {"_id": 3, "loc": {"type": "LineString", "coordinates": [[1, 1], [11, 1], [11, 11]]}}
    )
    chord = {"type": "LineString", "coordinates": [[1, 5], [11, 5]]}
    got = mongo_answer(
        lambda: [d["_id"] for d in db.c.find({"loc": {"$geoIntersects": {"$geometry": chord}}})]
    )
    assert got == ("ok", [])


@pytest.mark.backlog("RM-149")
def test_2dsphere_cell_levels_accepted_and_listed(rust_mongo: Any) -> None:
    """mongod 8.2.11 builds a `2dsphere` index with `coarsestIndexedLevel`
    and `finestIndexedLevel`, and lists both after the index version."""
    db = _fresh(rust_mongo, "rmgeo_l")
    spec = {
        "key": {"loc": "2dsphere"},
        "name": "s",
        "coarsestIndexedLevel": 1,
        "finestIndexedLevel": 29,
    }
    reply = mongo_answer(lambda: db.command("createIndexes", "c", indexes=[spec]))
    got = (reply[0], reply[1].get("numIndexesAfter") if reply[0] == "ok" else reply[1:])
    assert got == ("ok", 2)
    listed = [list(x.items()) for x in db.c.list_indexes() if x["name"] == "s"]
    assert listed == [
        [
            ("v", 2),
            ("key", {"loc": "2dsphere"}),
            ("name", "s"),
            ("2dsphereIndexVersion", 3),
            ("coarsestIndexedLevel", 1),
            ("finestIndexedLevel", 29),
        ]
    ]


@pytest.mark.backlog("RM-150")
def test_refused_spec_leaves_no_created_collection(rust_mongo: Any) -> None:
    """mongod 8.2.11: a `createIndexes` refused because a spec has no name
    creates nothing, neither the collection it would have created nor the
    spec before it."""
    db = _fresh(rust_mongo, "rmgeo_n")
    specs = [{"key": {"a": 1}, "name": "a"}, {"key": {"b": 1}}]
    got = mongo_answer(lambda: db.command("createIndexes", "n", indexes=specs))
    assert got == (
        "err",
        9,
        "Error in specification { key: { b: 1 } } :: caused by :: The 'name' field is a "
        "required property of an index specification",
    )
    assert "n" not in db.list_collection_names()
