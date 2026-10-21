"""RM-219: `setClusterParameter`, measured against mongod 8.2.11.

mongod keeps a parameter it is given in `config.clusterParameters`, as sent,
and `getClusterParameter` reads it back parsed. Of the fifteen parameters only
`defaultMaxTimeMS` acts on a single node: it limits the reads of an
authenticated user who lacks `bypassDefaultMaxTimeMS`.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pymongo
import pytest
from bson import Decimal128, Int64, Timestamp
from pymongo.errors import PyMongoError

from tests.backlog.test_rm_057_084 import auth_rust, mongod_rs  # noqa: F401 - fixture
from tests.mongod_reference import sort_type_lists


def answer(fn: Callable[[], Any]) -> tuple:
    """`("ok", value)` or `("err", code, message)`: the messages are part of
    what is compared."""
    try:
        return ("ok", fn())
    except PyMongoError as e:
        msg = getattr(e, "details", {}).get("errmsg")
        return ("err", getattr(e, "code", None), sort_type_lists(msg) if msg else msg)


DEFAULTS = [
    ("defaultMaxTimeMS", {"readOperations": 0}),
    ("changeStreams", {"expireAfterSeconds": 3600}),
    ("ddlLockOptimisticRecoveryWaitTimeout", {"timeInMs": 100}),
    ("internalQueryCutoffForSampleFromRandomCursor", {"sampleCutoff": 0.05}),
    ("internalSearchOptions", {"oversubscriptionFactor": 1.064, "batchSizeGrowthFactor": 1.5}),
    ("changeStreamOptions", {"preAndPostImages": {"expireAfterSeconds": "off"}}),
]

CASES: list[Any] = [
    {"defaultMaxTimeMS": {"readOperations": 5.5}},
    {"defaultMaxTimeMS": {"readOperations": Decimal128("7.9")}},
    {"defaultMaxTimeMS": {"readOperations": float("nan")}},
    {"defaultMaxTimeMS": {"readOperations": 1e20}},
    {"defaultMaxTimeMS": {"readOperations": -1}},
    {"defaultMaxTimeMS": {"readOperations": -1e20}},
    {"defaultMaxTimeMS": {"readOperations": "s"}},
    {"defaultMaxTimeMS": {"readOperations": None}},
    {"defaultMaxTimeMS": {}},
    {"defaultMaxTimeMS": 5},
    {"defaultMaxTimeMS": {"readOperations": 0, "x": 1}},
    {"defaultMaxTimeMS": {"readOperations": "s", "_id": 1}},
    {"defaultMaxTimeMS": {"readOperations": 0, "clusterParameterTime": Timestamp(5, 5)}},
    {"changeStreams": {"expireAfterSeconds": 0.5}},
    {"changeStreams": {"expireAfterSeconds": Decimal128("0.6")}},
    {"changeStreams": {"expireAfterSeconds": True}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": 5.0}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": 5.5}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": float("nan")}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": 1e21}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": Decimal128("7.9")}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": Decimal128("5")}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": None}},
    {"ddlLockOptimisticRecoveryWaitTimeout": {"timeInMs": "s"}},
    {"internalQueryCutoffForSampleFromRandomCursor": {"sampleCutoff": 1}},
    {"internalQueryCutoffForSampleFromRandomCursor": {"sampleCutoff": 0.0}},
    {"internalQueryCutoffForSampleFromRandomCursor": {"sampleCutoff": 5.5}},
    {"internalSearchOptions": {"batchSizeGrowthFactor": 0.5}},
    {"fleCompactionOptions": {"maxCompactionSize": Int64(5)}},
    {"fleCompactionOptions": {"maxESCEntriesPerCompactionDelete": 0}},
    {"onlyTargetDataOwningShardsForMultiWrites": {"enabled": 1}},
    {"onlyTargetDataOwningShardsForMultiWrites": {"enabled": True}},
    {"changeStreamOptions": {"preAndPostImages": {"expireAfterSeconds": 10}}},
    {"changeStreamOptions": {"preAndPostImages": {"expireAfterSeconds": "x"}}},
    {"changeStreamOptions": {"preAndPostImages": {"expireAfterSeconds": 0.4}}},
    {"changeStreamOptions": {"preAndPostImages": {"expireAfterSeconds": None}}},
    {"changeStreamOptions": {"preAndPostImages": {}}},
    {"changeStreamOptions": {"preAndPostImages": 5}},
    {"changeStreamOptions": {"x": 1, "preAndPostImages": 5}},
    {"changeStreamOptions": {}},
    {"nope": 5},
    {},
    {"defaultMaxTimeMS": {"readOperations": 0}, "changeStreams": {"expireAfterSeconds": 10}},
    [],
]


def _strip(d: dict) -> str:
    """A parameter without its time, which differs run to run; the time's
    type stays. As its repr, so that a stored NaN equals itself."""
    d = dict(d)
    t = d.pop("clusterParameterTime", None)
    return repr((d, type(t).__name__))


def _cluster_parameters(client: Any) -> list:
    admin = client.admin
    for name, body in DEFAULTS:
        admin.command("setClusterParameter", {name: body})
    out: list = []
    for case in CASES:
        got = answer(lambda case=case: admin.command("setClusterParameter", case))
        if got[0] == "ok":
            name = next(iter(case))
            got = (
                _strip(admin.command("getClusterParameter", name)["clusterParameters"][0]),
                _strip(client.config.clusterParameters.find_one({"_id": name})),
            )
        out.append(got)
    out.append(answer(lambda: client.test.command("setClusterParameter", {"defaultMaxTimeMS": {}})))
    out.append(
        answer(
            lambda: admin.command(
                "setClusterParameter", {"defaultMaxTimeMS": {}}, writeConcern={"w": 1}
            )
        )
    )
    # A document written to the collection is read back; one deleted is the
    # default again, with the null time.
    client.config.clusterParameters.update_one(
        {"_id": "defaultMaxTimeMS"}, {"$set": {"readOperations": 77}}
    )
    out.append(
        _strip(admin.command("getClusterParameter", "defaultMaxTimeMS")["clusterParameters"][0])
    )
    client.config.clusterParameters.delete_one({"_id": "defaultMaxTimeMS"})
    out.append(admin.command("getClusterParameter", "defaultMaxTimeMS")["clusterParameters"])
    out.append([_strip(p) for p in admin.command("getClusterParameter", "*")["clusterParameters"]])
    for name, body in DEFAULTS:
        admin.command("setClusterParameter", {name: body})
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_set_cluster_parameter_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """`setClusterParameter` checks a value as mongod 8.2.11 does (each
    field's types and bounds, unknown and duplicate fields, one parameter at
    a time), stores it as sent, and `getClusterParameter` reports it parsed."""
    assert _cluster_parameters(rust_mongo) == _cluster_parameters(mongod_rs)


def _reads(db: Any) -> dict:
    slow = {"$expr": {"$regexMatch": {"input": "$s", "regex": "9999$"}}}
    return {
        "find": answer(lambda: len(list(db.big.find(slow)))),
        "find, maxTimeMS 0": answer(lambda: len(list(db.big.find(slow).max_time_ms(0)))),
        "aggregate": answer(lambda: len(list(db.big.aggregate([{"$match": slow}])))),
        "aggregate $out": answer(
            lambda: len(list(db.big.aggregate([{"$match": slow}, {"$out": "o"}])))
        ),
        "count": answer(lambda: db.command("count", "big", query=slow)["n"]),
        "distinct": answer(
            lambda: len(db.command("distinct", "big", key="s", query=slow)["values"])
        ),
        "update": answer(lambda: db.big.update_many(slow, {"$set": {"z": 1}}).matched_count),
    }


@pytest.mark.backlog_fixed("RM-219")
def test_default_max_time_ms_limits_reads_without_bypass(tmp_path: Any) -> None:
    """With `defaultMaxTimeMS.readOperations: 1`, mongod 8.2.11 (`--auth`,
    measured over 200,000 documents) ended a `find`, an `aggregate`, a
    `count` and a `distinct` by a `readWrite` user with 50; the same user's
    `find` with `maxTimeMS: 0`, `aggregate` with `$out` and `update` ran,
    and so did every read by `root`, which holds `bypassDefaultMaxTimeMS`."""
    with auth_rust(tmp_path) as (host, port):
        root = pymongo.MongoClient(
            host=host, port=port, directConnection=True, username="root", password="pw"
        )
        alice = pymongo.MongoClient(
            host=host, port=port, directConnection=True, username="alice", password="pw"
        )
        root.p219.big.insert_many([{"i": i, "s": f"{'x' * 50}{i}"} for i in range(3_000)])
        root.admin.command("setClusterParameter", {"defaultMaxTimeMS": {"readOperations": 1}})
        limited = _reads(alice.p219)
        bypassed = _reads(root.p219)
        root.admin.command("setClusterParameter", {"defaultMaxTimeMS": {"readOperations": 0}})
        unlimited = _reads(alice.p219)
    expired = ("err", 50)
    assert {k: v[:2] if v[0] == "err" else "ok" for k, v in limited.items()} == {
        "find": expired,
        "find, maxTimeMS 0": "ok",
        "aggregate": expired,
        "aggregate $out": "ok",
        "count": expired,
        "distinct": expired,
        "update": "ok",
    }
    assert all(v[0] == "ok" for v in bypassed.values()), bypassed
    assert all(v[0] == "ok" for v in unlimited.values()), unlimited
