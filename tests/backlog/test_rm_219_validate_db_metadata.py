"""RM-219: `validateDBMetadata`, and the API Version 1 stage rules it and
`apiStrict` share, measured against mongod 8.2.11.
"""

from __future__ import annotations

from typing import Any

import pymongo
import pytest
from pymongo.errors import PyMongoError
from pymongo.server_api import ServerApi

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

STRICT = {"version": "1", "strict": True}


def _answer(fn: Any) -> tuple:
    try:
        got = fn()
        return ("ok", {k: v for k, v in got.items() if k not in ("$clusterTime", "operationTime")})
    except PyMongoError as e:
        return ("err", getattr(e, "code", None), (getattr(e, "details", None) or {}).get("errmsg"))


ITEMS: list[tuple[str, Any]] = [
    ("2d index", lambda db: db.c.create_index([("loc", "2d")], name="i")),
    ("2dsphere index", lambda db: db.c.create_index([("g", "2dsphere")], name="i")),
    (
        "partial index",
        lambda db: db.c.create_index("p", name="i", partialFilterExpression={"p": {"$gt": 1}}),
    ),
    ("unique index", lambda db: db.c.create_index("u", name="i", unique=True)),
    ("ttl index", lambda db: db.c.create_index("t", name="i", expireAfterSeconds=5)),
    ("hidden index", lambda db: db.c.create_index("h", name="i", hidden=True)),
    ("sparse index", lambda db: db.c.create_index("s", name="i", sparse=True)),
    (
        "sparse 2dsphere index",
        lambda db: db.c.create_index([("g", "2dsphere")], name="i", sparse=True),
    ),
    ("view", lambda db: db.command("create", "v", viewOn="c", pipeline=[{"$project": {"x": 1}}])),
    (
        "view with $fill",
        lambda db: db.command(
            "create", "v", viewOn="c", pipeline=[{"$fill": {"output": {"x": {"value": 0}}}}]
        ),
    ),
    (
        "view with $indexStats",
        lambda db: db.command("create", "v", viewOn="c", pipeline=[{"$indexStats": {}}]),
    ),
    (
        "view with a nested $fill",
        lambda db: db.command(
            "create",
            "v",
            viewOn="c",
            pipeline=[{"$facet": {"a": [{"$fill": {"output": {"x": {"value": 0}}}}]}}],
        ),
    ),
    (
        "view with $indexStats in a $lookup",
        lambda db: db.command(
            "create",
            "v",
            viewOn="c",
            pipeline=[{"$lookup": {"from": "c", "pipeline": [{"$indexStats": {}}], "as": "j"}}],
        ),
    ),
    (
        "validator",
        lambda db: db.command("create", "val", validator={"$jsonSchema": {"bsonType": "object"}}),
    ),
    ("time-series", lambda db: db.command("create", "ts", timeseries={"timeField": "t"})),
    ("capped", lambda db: db.command("create", "cap", capped=True, size=4096)),
]


def _metadata(client: Any) -> list:
    out = []
    for _label, make in ITEMS:
        client.drop_database("rm219v")
        db = client.rm219v
        db.c.insert_one({"x": 1, "loc": [0, 0], "g": {"type": "Point", "coordinates": [0, 0]}})
        make(db)
        out.append(
            _answer(
                lambda db=db: db.command(
                    {"validateDBMetadata": 1, "db": "rm219v", "apiParameters": STRICT}
                )
            )
        )
    db = client.rm219v
    db.c.create_index("s2", name="sp", sparse=True)
    for cmd in [
        {"validateDBMetadata": 1, "db": "rm219v", "collection": "c", "apiParameters": STRICT},
        {"validateDBMetadata": 1, "db": "rm219v", "collection": "nope", "apiParameters": STRICT},
        {"validateDBMetadata": 1, "db": "rm219v_none", "apiParameters": STRICT},
        {"validateDBMetadata": 1, "db": "rm219v", "apiParameters": {"version": "1"}},
        {
            "validateDBMetadata": 1,
            "db": "rm219v",
            "apiParameters": {"version": "2", "strict": True},
        },
        {"validateDBMetadata": 1},
        {"validateDBMetadata": 1, "db": 5, "apiParameters": STRICT},
        {"validateDBMetadata": 1, "x": 1, "apiParameters": STRICT},
    ]:
        out.append(_answer(lambda cmd=cmd: client.admin.command(cmd)))
    client.drop_database("rm219v")
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_validate_db_metadata_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """A sparse index is listed as outside API Version 1; a view using a
    stage outside it (nested ones included) fails the command with 323;
    other index types and collection kinds pass."""
    assert _metadata(rust_mongo) == _metadata(mongod_rs)


def _strict_stages(client: Any) -> list:
    host, port = client.address
    strict = pymongo.MongoClient(
        host, port, directConnection=True, server_api=ServerApi("1", strict=True)
    )
    client.rm219v.c.insert_one({"x": 1})
    out = []
    for pipeline in [
        [{"$indexStats": {}}],
        [{"$fill": {"output": {"x": {"value": 0}}}}],
        [{"$densify": {"field": "x", "range": {"step": 1, "bounds": "full"}}}],
        [{"$facet": {"a": [{"$fill": {"output": {"x": {"value": 0}}}}]}}],
        [{"$lookup": {"from": "c", "pipeline": [{"$indexStats": {}}], "as": "j"}}],
        [{"$unionWith": {"coll": "c", "pipeline": [{"$fill": {"output": {"x": {"value": 0}}}}]}}],
    ]:
        out.append(_answer(lambda p=pipeline: {"n": len(list(strict.rm219v.c.aggregate(p)))}))
    strict.close()
    client.drop_database("rm219v")
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_api_strict_refuses_fill_and_index_stats(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """Under `apiStrict: true` mongod 8.2.11 refuses `$fill` and `$indexStats`
    (323), as it does a stage outside API Version 1 inside `$facet`,
    `$lookup` or `$unionWith`; this server ran all of them."""
    assert _strict_stages(rust_mongo) == _strict_stages(mongod_rs)
