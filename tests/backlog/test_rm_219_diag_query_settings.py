"""RM-219's last commands against mongod 8.2: `getDiagnosticData`, and the
query settings (`setQuerySettings`, `removeQuerySettings`, `$querySettings`).

Each test asks a one-member mongod replica set (`mongod_rs`) live and asserts
the Rust server answers the same, reduced to what the test is about.
"""

from __future__ import annotations

import os
import re
import time
from typing import Any

import bson
import pymongo
import pytest

from tests.backlog.conftest import OS, only_on, rust_mongo_server
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# --------------------------------------------------- getDiagnosticData --


def _latest_sample(client: Any) -> dict:
    """A sample taken after this call began: the one answered first may
    predate the fixture's set initiation."""
    first = client.admin.command("getDiagnosticData")["data"]
    deadline = time.monotonic() + 10
    while True:
        data = client.admin.command("getDiagnosticData")["data"]
        if data and data.get("start") != first.get("start"):
            return data
        assert time.monotonic() < deadline, "no new FTDC sample within 10 s"
        time.sleep(0.05)


def _sample_shape(client: Any) -> dict:
    data = _latest_sample(client)
    collectors = {k: v for k, v in data.items() if isinstance(v, dict)}
    return {
        "keys": list(data),
        "stamped": {k: (next(iter(v)), list(v)[-1]) for k, v in collectors.items()},
        "set": data["replSetGetStatus"].get("ok"),
        "process": data["serverStatus"].get("process"),
    }


@pytest.mark.backlog_fixed("RM-219")
def test_get_diagnostic_data_samples_mongods_collectors(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """A set member's sample is `start`, `replSetGetStatus`, the three
    `$collStats` collectors, `serverStatus` and `end`, each collector stamped
    with its own `start` and `end` -- on each OS, as its mongod samples: on
    Linux mongod adds `systemMetrics`, which RM-424's test compares."""
    ours, theirs = _sample_shape(rust_mongo), _sample_shape(mongod_rs)
    if OS == "linux":
        for shape in (ours, theirs):
            shape["keys"] = [k for k in shape["keys"] if k != "systemMetrics"]
            shape["stamped"].pop("systemMetrics", None)
    assert ours == theirs


@pytest.mark.backlog_fixed("RM-424")
def test_linux_diagnostic_data_samples_system_metrics(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """On Linux mongod 8.2's diagnostic sample carries a `systemMetrics`
    collector (the machine's CPU, memory and disk counters from `/proc`),
    stamped with its own `start` and `end`, just before `end` (CI,
    2026-11-08). This server's had none; it now reads the same `/proc`
    files. macOS mongod has no such collector."""
    only_on("linux", why="macOS mongod samples no systemMetrics")

    def system_metrics(client: Any) -> tuple:
        shape = _sample_shape(client)
        return ("systemMetrics" in shape["keys"], shape["stamped"].get("systemMetrics"))

    assert system_metrics(rust_mongo) == system_metrics(mongod_rs)


def _refreshes_by_period(client: Any) -> tuple:
    """Whether a call answers the kept sample (two calls in a row agree at
    least once) and whether a later period brings a later one."""
    first = _latest_sample(client)
    repeated = False
    deadline = time.monotonic() + 5
    later = first
    while time.monotonic() < deadline and (not repeated or later["start"] == first["start"]):
        a = client.admin.command("getDiagnosticData")["data"]
        b = client.admin.command("getDiagnosticData")["data"]
        repeated = repeated or a == b
        later = b
        time.sleep(0.05)
    return repeated, later["start"] > first["start"]


@pytest.mark.backlog_fixed("RM-219")
def test_get_diagnostic_data_answers_the_latest_periodic_sample(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod answers the sample its collector last took, not a fresh one,
    and takes one every `diagnosticDataCollectionPeriodMillis` (1000)."""
    assert _refreshes_by_period(rust_mongo) == _refreshes_by_period(mongod_rs) == (True, True)


# ------------------------------------------------------ query settings --

Q = {"find": "a", "filter": {"x": 1}, "$db": "rm219q"}
NS = {"db": "rm219q", "coll": "a"}


def _reply(client: Any, cmd: dict, db: str = "admin") -> tuple:
    try:
        r = client[db].command(cmd)
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code, e.details.get("errmsg"))
    return ("ok", {k: v for k, v in r.items() if k not in ("$clusterTime", "operationTime")})


def _clear(client: Any) -> None:
    """Remove every setting: they outlive a test on a module's server."""
    for e in list(client.admin.aggregate([{"$querySettings": {}}])):
        client.admin.command({"removeQuerySettings": e["queryShapeHash"]})


def _set(settings: Any, query: Any = None) -> dict:
    return {"setQuerySettings": Q if query is None else query, "settings": settings}


SET_CASES: list[tuple[str, dict]] = [
    ("no settings", {"setQuerySettings": Q}),
    ("settings not an object", _set(5)),
    ("empty settings", _set({})),
    ("unknown settings field", _set({"bogus": 1})),
    ("reject false only", _set({"reject": False})),
    ("reject not a bool", _set({"reject": 1})),
    ("comment only", _set({"comment": "x"})),
    ("framework unknown", _set({"queryFramework": "bogus"})),
    ("framework not a string", _set({"queryFramework": 1})),
    ("hints not an object", _set({"indexHints": 5})),
    ("hint without ns", _set({"indexHints": {"allowedIndexes": ["x_1"]}})),
    ("hint ns not an object", _set({"indexHints": {"ns": 5, "allowedIndexes": ["x"]}})),
    ("hint ns unknown field", _set({"indexHints": {"ns": dict(NS, q=1), "allowedIndexes": ["x"]}})),
    ("hint ns without db", _set({"indexHints": {"ns": {"coll": "a"}, "allowedIndexes": ["x"]}})),
    (
        "hint ns without coll",
        _set({"indexHints": {"ns": {"db": "rm219q"}, "allowedIndexes": ["x"]}}),
    ),
    ("hint without allowed", _set({"indexHints": {"ns": NS}})),
    ("allowed not an array", _set({"indexHints": {"ns": NS, "allowedIndexes": "x_1"}})),
    ("allowed bad element", _set({"indexHints": {"ns": NS, "allowedIndexes": [5]}})),
    (
        "hints twice for one collection",
        _set(
            {
                "indexHints": [
                    {"ns": NS, "allowedIndexes": ["x_1"]},
                    {"ns": NS, "allowedIndexes": ["y_1"]},
                ]
            }
        ),
    ),
    ("argument not a string or object", {"setQuerySettings": 5, "settings": {"reject": True}}),
    ("hash not hex", {"setQuerySettings": "xyz", "settings": {"reject": True}}),
    ("hash too short", {"setQuerySettings": "AB", "settings": {"reject": True}}),
    ("unknown command field", dict(_set({"reject": True}), foo=1)),
    ("not a read", _set({"reject": True}, {"insert": "a", "$db": "rm219q"})),
    ("no $db", _set({"reject": True}, {"find": "a", "filter": {}})),
    ("$db not a string", _set({"reject": True}, {"find": "a", "$db": 5})),
    ("collection not a string", _set({"reject": True}, {"find": 5, "$db": "rm219q"})),
    ("unknown find field", _set({"reject": True}, {"find": "a", "bogus": 1, "$db": "rm219q"})),
    (
        "unknown distinct field",
        _set({"reject": True}, {"distinct": "a", "key": "x", "zz": 1, "$db": "rm219q"}),
    ),
    (
        "unknown aggregate field",
        _set({"reject": True}, {"aggregate": "a", "pipeline": [], "zz": 1, "$db": "rm219q"}),
    ),
    (
        "unknown top-level operator",
        _set({"reject": True}, {"find": "a", "filter": {"$foo": 1}, "$db": "rm219q"}),
    ),
    ("internal database", _set({"reject": True}, {"find": "a", "filter": {}, "$db": "admin"})),
    (
        "system collection",
        _set({"reject": True}, {"find": "system.js", "filter": {}, "$db": "rm219q"}),
    ),
    (
        "IDHACK",
        _set(
            {"reject": True}, {"find": "a", "filter": {"_id": 1}, "sort": {"a": 1}, "$db": "rm219q"}
        ),
    ),
    (
        "IDHACK by $eq",
        _set({"reject": True}, {"find": "a", "filter": {"_id": {"$eq": 1}}, "$db": "rm219q"}),
    ),
    ("remove: hash not hex", {"removeQuerySettings": "xyz"}),
    ("remove: hash too short", {"removeQuerySettings": "AB"}),
    ("remove: unknown field", {"removeQuerySettings": "0" * 64, "foo": 1}),
    ("remove: unknown hash", {"removeQuerySettings": "0" * 64}),
    (
        "remove: IDHACK query",
        {"removeQuerySettings": {"find": "a", "filter": {"_id": 1}, "$db": "rm219q"}},
    ),
    ("on another database", ({"setQuerySettings": Q, "settings": {"reject": True}}, "rm219q")),
]


def _set_refusals(client: Any) -> list:
    client.drop_database("rm219q")
    _clear(client)
    out = []
    for name, cmd in SET_CASES:
        db = "admin"
        if isinstance(cmd, tuple):
            cmd, db = cmd
        out.append((name, _reply(client, cmd, db)))
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_set_query_settings_refuses_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """Every argument mongod 8.2 refuses, with its code and message, and the
    ones it takes without a change (an unknown hash removed, an IDHACK query
    removed)."""
    assert _set_refusals(rust_mongo) == _set_refusals(mongod_rs)


def _settings_kept(client: Any) -> dict:
    client.drop_database("rm219q")
    _clear(client)
    hash_q = _reply(client, _set({"indexHints": {"ns": NS, "allowedIndexes": ["x_1"]}}))[1][
        "queryShapeHash"
    ]
    steps = [
        _set({"indexHints": {"ns": NS, "allowedIndexes": ["x_1"]}}),
        _set({"reject": True}),
        {"setQuerySettings": hash_q.lower(), "settings": {"comment": {"a": 1}}},
        _set({"queryFramework": "classic"}),
        _set({"reject": False}),
        _set(
            {
                "indexHints": [
                    {"ns": NS, "allowedIndexes": []},
                    {"ns": dict(NS, coll="b"), "allowedIndexes": [{"y": 1}, {"$natural": -1}]},
                ]
            }
        ),
        {"setQuerySettings": "AB" * 32, "settings": {"reject": True}},
        _set({"reject": True}, {"distinct": "a", "key": "x", "query": {"y": 2}, "$db": "rm219q"}),
        _set(
            {"reject": True},
            {"aggregate": "a", "pipeline": [{"$match": {"x": 2}}], "cursor": {}, "$db": "rm219q"},
        ),
        _set(
            {"reject": True},
            {"find": "a", "filter": {"x": 3}, "limit": 2, "comment": "c", "$db": "rm219q"},
        ),
    ]
    replies = [_reply(client, s) for s in steps]
    listed = sorted(
        client.admin.aggregate([{"$querySettings": {}}]), key=lambda e: e["queryShapeHash"]
    )
    status = client.admin.command("serverStatus")["querySettings"]
    row = client.config.clusterParameters.find_one({"_id": "querySettings"})
    stored = sorted(row["settingsArray"], key=lambda e: e["queryShapeHash"])
    removed = _reply(client, {"removeQuerySettings": "AB" * 32})
    after = client.admin.command("serverStatus")["querySettings"]
    return {
        "replies": replies,
        "listed": listed,
        "status": status,
        "stored": stored,
        "stamped": isinstance(row.get("clusterParameterTime"), bson.Timestamp),
        "removed": removed,
        "after": after,
    }


@pytest.mark.backlog_fixed("RM-219")
def test_query_settings_merge_and_are_kept_as_mongods(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """Each reply (`queryShapeHash`, `settings` in mongod's order,
    `representativeQuery`), the merging of a second set for one shape, the
    `$querySettings` list, `serverStatus.querySettings` and the cluster
    parameter document that keeps them."""
    assert _settings_kept(rust_mongo) == _settings_kept(mongod_rs)


def _winning(client: Any, cmd: dict) -> tuple:
    e = client.rm219p.command("explain", cmd, verbosity="queryPlanner")
    qp = e["queryPlanner"] if "queryPlanner" in e else e["stages"][0]["$cursor"]["queryPlanner"]
    w = qp["winningPlan"]
    w = w.get("queryPlan", w)

    def stage(p: dict) -> str:
        child = p.get("inputStage")
        name = p["stage"]
        if "indexName" in p:
            name += ":" + p["indexName"]
        if p["stage"] == "COLLSCAN":
            name += ":" + p["direction"]
        return name + (f"<{stage(child)}>" if child else "")

    return stage(w), qp.get("querySettings"), qp.get("indexFilterSet")


def _settings_applied(client: Any) -> dict:
    client.drop_database("rm219p")
    _clear(client)
    db = client.rm219p
    db.a.insert_many([{"_id": i, "x": i, "y": i} for i in range(50)])
    db.a.create_index("x")
    db.a.create_index("y")
    ns = {"db": "rm219p", "coll": "a"}
    find = {"find": "a", "filter": {"x": 3, "y": 3}}
    only_x = {"find": "a", "filter": {"x": 3}}

    def hints(query: dict, *allowed: Any) -> None:
        client.admin.command(
            {
                "setQuerySettings": dict(query, **{"$db": "rm219p"}),
                "settings": {"indexHints": {"ns": ns, "allowedIndexes": list(allowed)}},
            }
        )

    out: dict = {"before": _winning(client, find)}
    hints(find, "y_1")
    out["y_1"] = _winning(client, find)
    out["y_1, another literal"] = _winning(client, {"find": "a", "filter": {"x": 9, "y": 10}})
    out["y_1 over a hint"] = _winning(client, dict(find, hint="x_1"))
    out["y_1 rows"] = db.command(find)["cursor"]["firstBatch"]
    hints(find, "nonexistent")
    out["no such index"] = _winning(client, find)
    hints(find, {"$natural": 1})
    out["$natural"] = _winning(client, find)
    hints(find, {"$natural": -1}, {"x": 1})
    out["$natural and x"] = _winning(client, find)
    hints(only_x, "y_1")
    out["an index that cannot serve it"] = _winning(client, only_x)
    hints(only_x, {"$natural": -1})
    out["$natural backward"] = _winning(client, only_x)
    hints(only_x, "y_1", {"$natural": 1})
    out["an unusable index and $natural"] = _winning(client, only_x)
    db.command({"planCacheSetFilter": "a", "query": {"x": 1, "y": 1}, "indexes": ["x_1"]})
    client.admin.command(
        {
            "setQuerySettings": dict(find, **{"$db": "rm219p"}),
            "settings": {"comment": "c", "queryFramework": "classic"},
        }
    )
    out["index filter under settings"] = _winning(client, find)
    client.admin.command(
        {"setQuerySettings": dict(find, **{"$db": "rm219p"}), "settings": {"reject": True}}
    )
    out["rejected"] = _reply(client, find, "rm219p")
    out["rejected explain"] = _winning(client, find)
    out["another shape runs"] = _reply(
        client, {"find": "a", "filter": {"x": 3}, "limit": 1}, "rm219p"
    )[0]
    distinct = {"distinct": "a", "key": "x", "query": {"y": {"$gt": 1}}}
    agg = {"aggregate": "a", "pipeline": [{"$match": {"y": 4}}], "cursor": {}}
    for cmd in (distinct, agg):
        client.admin.command(
            {"setQuerySettings": dict(cmd, **{"$db": "rm219p"}), "settings": {"reject": True}}
        )
    out["distinct rejected"] = _reply(client, distinct, "rm219p")
    out["aggregate rejected"] = _reply(client, agg, "rm219p")
    client.admin.command({"removeQuerySettings": dict(find, **{"$db": "rm219p"})})
    out["removed"] = _winning(client, find)
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_query_settings_apply_to_the_reads_of_their_shape(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """A read of a shape with settings plans over its allowed indexes
    (`$natural` the scan, in its direction; nothing usable: as without
    hints), ignores its own `hint` and the index filters, says so in its
    `explain`, and is refused with 411 under `reject: true`."""
    assert _settings_applied(rust_mongo) == _settings_applied(mongod_rs)


def _stage_refusals(client: Any) -> list:
    cases = [
        ("not an object", {"aggregate": 1, "pipeline": [{"$querySettings": 1}], "cursor": {}}),
        (
            "unknown option",
            {"aggregate": 1, "pipeline": [{"$querySettings": {"foo": 1}}], "cursor": {}},
        ),
        ("on a collection", {"aggregate": "x", "pipeline": [{"$querySettings": {}}], "cursor": {}}),
        (
            "not first",
            {"aggregate": 1, "pipeline": [{"$match": {}}, {"$querySettings": {}}], "cursor": {}},
        ),
    ]
    return [(name, _reply(client, cmd)) for name, cmd in cases]


@pytest.mark.backlog_fixed("RM-219")
def test_query_settings_stage_refuses_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """`$querySettings` takes `{showDebugQueryShape}` and runs first in an
    `{aggregate: 1}`."""
    assert _stage_refusals(rust_mongo) == _stage_refusals(mongod_rs)


# ------------------------------------------- found while working RM-219 --


def _ftdc_parameters(client: Any) -> tuple:
    def get(name: str) -> Any:
        try:
            return client.admin.command({"getParameter": 1, name: 1})[name]
        except pymongo.errors.OperationFailure as e:
            return ("err", e.code)

    return (
        get("diagnosticDataCollectionEnabled"),
        get("diagnosticDataCollectionPeriodMillis"),
        "transportLayerStats" in _latest_sample(client),
    )


@pytest.mark.backlog_fixed("RM-417")
def test_ftdc_parameters_and_transport_collector(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod reports `diagnosticDataCollectionEnabled` (true) and
    `diagnosticDataCollectionPeriodMillis` (1000), and its sample carries a
    `transportLayerStats` collector. This server knew neither parameter and
    had no such collector."""
    assert _ftdc_parameters(rust_mongo) == _ftdc_parameters(mongod_rs)


def _transport_layer_stats(client: Any) -> tuple:
    stats = _latest_sample(client)["transportLayerStats"]
    port = client.address[1]
    return (
        list(stats),
        # mongod also listens on its Unix domain socket (RM-417).
        [d for d in stats["listenerSocketBacklogQueueDepths"] if not next(iter(d)).startswith("/")],
        type(stats["connsDiscardedDueToClientDisconnect"]),
        port,
    )


@pytest.mark.backlog_fixed("RM-417")
def test_transport_layer_stats_name_the_tcp_listener(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11's `transportLayerStats` is stamped `start` / `end`
    around each listener's backlog depth, keyed `host:port` (0 on macOS,
    where mongod does not measure it), and the connections discarded for a
    client that left."""
    ours, theirs = _transport_layer_stats(rust_mongo), _transport_layer_stats(mongod_rs)
    for shape in (ours, theirs):
        port = shape[3]
        assert shape[1] == [{f"127.0.0.1:{port}": 0}], shape
    assert (ours[0], ours[2]) == (theirs[0], theirs[2])


def _period_takes_effect(client: Any) -> tuple:
    def set_period(ms: int) -> Any:
        return client.admin.command({"setParameter": 1, "diagnosticDataCollectionPeriodMillis": ms})

    def start() -> Any:
        return client.admin.command("getDiagnosticData")["data"].get("start")

    try:
        set_period(60_000)
        time.sleep(1.2)
        before = start()
        time.sleep(1.2)
        held = start() == before
        set_period(200)
        deadline = time.monotonic() + 3
        while start() == before:
            assert time.monotonic() < deadline, "the new period took no effect within 3 s"
            time.sleep(0.02)
        client.admin.command({"setParameter": 1, "diagnosticDataCollectionEnabled": False})
        time.sleep(0.5)
        stopped = start()
        time.sleep(0.6)
        off = start() == stopped and stopped is not None
    finally:
        set_period(1000)
        client.admin.command({"setParameter": 1, "diagnosticDataCollectionEnabled": True})
    return held, off


@pytest.mark.backlog_fixed("RM-417")
def test_the_period_and_the_switch_take_effect(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """On mongod 8.2.11 a longer period holds the sample, a shorter one
    takes effect at once, and switching sampling off keeps the last sample."""
    assert _period_takes_effect(rust_mongo) == _period_takes_effect(mongod_rs) == (True, True)


def _ftdc_files(client: Any, dbpath: str) -> tuple:
    directory = os.path.join(dbpath, "diagnostic.data")
    try:
        reported = client.admin.command(
            {"getParameter": 1, "diagnosticDataCollectionDirectoryPath": 1}
        )["diagnosticDataCollectionDirectoryPath"]
    except pymongo.errors.OperationFailure as e:
        reported = ("err", e.code)
    names = os.listdir(directory) if os.path.isdir(directory) else []
    pattern = r"metrics\.\d{4}-\d\d-\d\dT\d\d-\d\d-\d\dZ-\d{5}"
    return reported == directory, any(re.fullmatch(pattern, n) for n in names)


@pytest.mark.backlog("RM-417")
def test_samples_are_written_to_diagnostic_data(
    tmp_path: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 writes its samples to `<dbpath>/diagnostic.data`, a
    `metrics.<time>-00000` file it names in
    `diagnosticDataCollectionDirectoryPath`."""
    dbpath = mongod_rs.admin.command("getCmdLineOpts")["parsed"]["storage"]["dbPath"]
    store = str(tmp_path / "store")
    with rust_mongo_server(store) as rust:
        _latest_sample(rust)
        ours = _ftdc_files(rust, store)
    assert ours == _ftdc_files(mongod_rs, dbpath) == (True, True)


def _framework_and_debug_shape(client: Any) -> tuple:
    client.drop_database("rm219f")
    _clear(client)
    client.rm219f.a.insert_many([{"x": i} for i in range(5)])
    query = {"find": "a", "filter": {"x": 1}}
    client.admin.command(
        {
            "setQuerySettings": dict(query, **{"$db": "rm219f"}),
            "settings": {"queryFramework": "sbe"},
        }
    )
    version = client.rm219f.command("explain", query)["explainVersion"]
    debug = [
        e.get("debugQueryShape")
        for e in client.admin.aggregate([{"$querySettings": {"showDebugQueryShape": True}}])
    ]
    _clear(client)
    return version, debug


@pytest.mark.backlog("RM-418")
def test_query_framework_chooses_the_engine_and_debug_shape_is_shown(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """Under `queryFramework: "sbe"` mongod runs a plain `find` in its
    slot-based engine (`explainVersion` 2), and `showDebugQueryShape` adds
    each query's `debugQueryShape`."""
    assert _framework_and_debug_shape(rust_mongo) == _framework_and_debug_shape(mongod_rs)


def _oplog_storage_stats(client: Any) -> tuple:
    client.rm219o.a.insert_one({})
    try:
        rows = list(client.local["oplog.rs"].aggregate([{"$collStats": {"storageStats": {}}}]))
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code)
    stats = rows[0]["storageStats"]
    return ("ok", stats.get("capped"), stats.get("count", 0) > 0, stats.get("size", 0) > 0)


@pytest.mark.backlog("RM-419")
def test_oplog_storage_stats(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """`$collStats: {storageStats: {}}` on `local.oplog.rs` answers, as for
    any collection: capped, with its entries counted and sized."""
    assert _oplog_storage_stats(rust_mongo) == _oplog_storage_stats(mongod_rs)


def _admin_remainders(client: Any) -> dict:
    db = client["rm219r"]
    client.drop_database("rm219r")
    db.a.insert_one({"_id": 1})
    db.fs.chunks.create_index([("files_id", 1), ("n", 1)], unique=True)
    db.fs.chunks.insert_one({"files_id": 1, "n": 0, "data": bson.Binary(b"abc")})

    def code(fn: Any) -> Any:
        try:
            fn()
            return "ok"
        except pymongo.errors.PyMongoError as e:
            return getattr(e, "code", type(e).__name__)

    big = {"_id": 2, "s": "x" * (16 * 1024 * 1024 + 1024)}
    return {
        "applyOps over 16 MB": code(
            lambda: client.admin.command("applyOps", [{"op": "i", "ns": "rm219r.a", "o": big}])
        ),
        "dbCheck extraIndexKeysCheck": code(
            lambda: db.command(
                {"dbCheck": "a", "validateMode": "extraIndexKeysCheck", "secondaryIndex": "_id_"}
            )
        ),
        "filemd5 partialOk": code(
            lambda: db.command({"filemd5": 1, "root": "fs", "partialOk": True})
        ),
    }


@pytest.mark.backlog("RM-416")
def test_admin_command_remainders(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2 applies an `applyOps` insert of a document a little over
    16 MB, runs `dbCheck`'s `extraIndexKeysCheck` and answers `filemd5`
    with `partialOk`."""
    assert _admin_remainders(rust_mongo) == _admin_remainders(mongod_rs)
