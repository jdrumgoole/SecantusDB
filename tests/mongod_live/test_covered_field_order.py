"""A covered row's fields in the index key's order, against mongod 8.2 (RM-569).

mongod builds a row a covered plan answers from the index key, so its fields
come in the key pattern's order: `{_id: 0, x: 1, y: 1}` over `{y: 1, x: 1}`
returns `{y, x}`. Under an `$or`, a row from an unfetched branch is built of
its key, one from a fetched branch keeps the document's order. The rows are
compared as a set (their order is RM-572's), each with its field order.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

YX = [("y", 1), ("x", 1)]
W = [("w", 1)]
XY_OUT = {"_id": 0, "x": 1, "y": 1}

CASES: dict[str, tuple[dict, list]] = {
    "one scan": ({"filter": {"y": 0}, "projection": XY_OUT}, [YX]),
    "one scan, getMore": (
        {"filter": {"y": {"$gte": 0}}, "projection": XY_OUT, "batchSize": 2},
        [YX],
    ),
    "one scan, a limit": ({"filter": {"y": 1}, "projection": XY_OUT, "limit": 2}, [YX]),
    "an $or of unfetched branches": (
        {"filter": {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}]}, "projection": XY_OUT},
        [YX],
    ),
    "an $or with a fetched branch": (
        {"filter": {"$or": [{"y": 0}, {"w": 1}]}, "projection": XY_OUT},
        [YX, W],
    ),
    "not covered: a fetch": ({"filter": {"y": 0, "z": 1}, "projection": XY_OUT}, [YX]),
}


def _rows(client: Any, command: dict, indexes: list) -> list:
    db = client["covered_order_" + uuid.uuid4().hex[:8]]
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2, "w": n % 4} for n in range(12)])
    for index in indexes:
        db.c.create_index(index)
    try:
        reply = db.command({"find": "c", **command})
        rows = list(reply["cursor"]["firstBatch"])
        cursor_id = reply["cursor"]["id"]
        while cursor_id:
            more = db.command({"getMore": cursor_id, "collection": "c"})
            rows += more["cursor"]["nextBatch"]
            cursor_id = more["cursor"]["id"]
        return sorted(repr(list(row.items())) for row in rows)
    finally:
        client.drop_database(db.name)


@pytest.mark.parametrize("case", list(CASES))
def test_a_covered_row_has_the_key_patterns_field_order(
    rust_mongo: Any, mongod: Any, case: str
) -> None:
    command, indexes = CASES[case]
    assert _rows(rust_mongo, command, indexes) == _rows(mongod, command, indexes)
