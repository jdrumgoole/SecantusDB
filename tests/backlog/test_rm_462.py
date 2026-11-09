"""Gap test for RM-462: a time-series measurement holding a BSONColumn binary
(subtype 7), which mongod 8.2.11 refuses."""

from __future__ import annotations

import uuid
from typing import Any

import bson
import pytest

from tests.backlog.conftest import mongo_answer


def _insert_column(client: Any, value: Any) -> tuple:
    db = client[f"rm462_{uuid.uuid4().hex[:8]}"]
    db.create_collection("ts", timeseries={"timeField": "t"})
    got = mongo_answer(
        lambda: db.ts.insert_one({"t": bson.datetime.datetime(2026, 1, 1), "v": value}).acknowledged
    )
    if got[0] == "err":
        got = (got[0], got[1], str(got[2]).replace(db.name, "@db"))
    stored = db.ts.count_documents({})
    client.drop_database(db.name)
    return got, stored


@pytest.mark.backlog("RM-462")
def test_a_measurement_cannot_hold_a_bsoncolumn(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses a data field that is, or holds, a binary of
    subtype 7, and stores nothing (the Rust server inserts it)."""
    column = bson.Binary(b"\x10\x00\x01\x00\x00\x00\x00", 7)
    refused = (
        "err",
        12506300,
        "time-series insert failed: @db.ts :: caused by :: Timeseries collections cannot "
        "store BSONColumn data",
    )
    for value in (column, {"x": column}):
        assert _insert_column(mongod, value) == (refused, 0)
        assert _insert_column(rust_mongo, value) == (refused, 0)
