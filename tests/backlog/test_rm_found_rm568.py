"""Divergences found while keeping `{$meta: "sortKey"}` under `returnKey`
(RM-568, 2026-11-15), each measured against mongod 8.2.11."""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _first_batch(client: Any, command: dict) -> Any:
    db = client["rm_found_568_" + uuid.uuid4().hex[:8]]
    db.c.insert_many([{"_id": 1, "x": [3, 1]}, {"_id": 2, "x": 2}])
    try:
        return db.command(command)["cursor"]["firstBatch"]
    except Exception as e:  # the refusal is the answer being compared
        return (getattr(e, "code", None), str(getattr(e, "details", {}).get("errmsg")))
    finally:
        client.drop_database(db.name)


@pytest.mark.backlog("RM-703")
def test_find_keeps_a_sort_key_meta_field(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 gives each document its sort key as an array, the
    smallest element of an array under an ascending sort. This server
    omits the field."""
    command = {
        "find": "c",
        "filter": {},
        "sort": {"x": 1},
        "projection": {"k": {"$meta": "sortKey"}},
    }
    answers = {s: _first_batch(c, command) for s, c in (("mongod", mongod), ("rust", rust_mongo))}
    assert answers["mongod"] == [{"_id": 1, "x": [3, 1], "k": [1]}, {"_id": 2, "x": 2, "k": [2]}]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-568")
@pytest.mark.parametrize("return_key", [False, True])
def test_a_sort_key_meta_field_without_a_sort_is_refused(
    rust_mongo: Any, mongod: Any, return_key: bool
) -> None:
    """mongod 8.2.11 refuses `{$meta: "sortKey"}` in a read with no sort,
    `returnKey` or not (BadValue). This server returned the documents."""
    command = {"find": "c", "filter": {}, "projection": {"k": {"$meta": "sortKey"}}}
    if return_key:
        command["returnKey"] = True
    answers = {s: _first_batch(c, command) for s, c in (("mongod", mongod), ("rust", rust_mongo))}
    assert answers["mongod"] == (2, "cannot use sortKey $meta projection without a sort")
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-568")
def test_return_key_keeps_sort_keys_beside_an_index_key(rust_mongo: Any, mongod: Any) -> None:
    """Under `returnKey` mongod 8.2.11 puts each `sortKey` field after the
    index key, and one named like an index field takes that field's place.
    This server returned the index key alone."""

    def answer(client: Any) -> list:
        db = client["rm_found_568_" + uuid.uuid4().hex[:8]]
        db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
        db.c.create_index([("y", 1), ("x", 1)])
        db.c.create_index([("y", 1), ("x", 1), ("z", 1)])
        try:
            return [
                db.command(
                    {
                        "find": "c",
                        "filter": {"y": 1},
                        "sort": sort,
                        "projection": proj,
                        "returnKey": True,
                    }
                )["cursor"]["firstBatch"][:3]
                for sort, proj in (
                    ({"z": 1}, {"k": {"$meta": "sortKey"}}),
                    ({"x": 1}, {"k": {"$meta": "sortKey"}, "y": {"$meta": "sortKey"}}),
                    ({"z": 1, "x": -1}, {"k": {"$meta": "sortKey"}, "j": {"$meta": "sortKey"}}),
                )
            ]
        finally:
            client.drop_database(db.name)

    answers = {s: answer(c) for s, c in (("mongod", mongod), ("rust", rust_mongo))}
    assert answers["mongod"][0][0] == {"y": 1, "x": 4, "z": 0, "k": [0]}
    assert answers["mongod"][1][0] == {"y": [1], "x": 1, "k": [1]}
    assert answers["rust"] == answers["mongod"]
