"""Gaps found 2026-11-17 while working RM-358 (`queryShapeHash` for a
cross-database `$lookup` and a time-series `$out`), each against a
one-member mongod 8.2.11 replica set."""

from __future__ import annotations

from typing import Any

import pymongo
import pytest

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

DB = "rm358found"


def _outcome(client: Any, pipeline: list) -> Any:
    client.drop_database(DB)
    db = client[DB]
    db.c.insert_many([{"x": 1}, {"x": 2}])
    try:
        return ("ok", len(list(db.c.aggregate(pipeline))))
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code)
    finally:
        client.drop_database(DB)


@pytest.mark.backlog("RM-717")
def test_a_lookup_from_the_oplog_runs(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod runs a `$lookup` from `{db: "local", coll: "oplog.rs"}`."""
    pipeline = [
        {
            "$lookup": {
                "from": {"db": "local", "coll": "oplog.rs"},
                "as": "j",
                "pipeline": [{"$limit": 1}],
            }
        }
    ]
    assert _outcome(mongod_rs, pipeline) == ("ok", 2)
    assert _outcome(rust_mongo, pipeline) == ("ok", 2)


@pytest.mark.backlog("RM-718")
def test_out_to_time_series_needs_the_time_field(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod refuses (BadValue) to `$out` a document with no `timeField`
    date to a new time-series collection."""
    pipeline = [{"$out": {"db": DB, "coll": "ts", "timeseries": {"timeField": "t"}}}]
    assert _outcome(mongod_rs, pipeline) == ("err", 2)
    assert _outcome(rust_mongo, pipeline) == ("err", 2)
