"""Gap tests for `tasks/backlog-rust-mongodb.md`, RM-029 to RM-056.

Sections 7.083 (user and role commands), 7.082 (handshake and diagnostic
commands), 7.081 (geo queries) and 7.080 (the order `find` returns
documents in). The reference is mongod 8.2.11: a literal where the answer is
stable (and, where the Rust server says it is a replica-set member, measured
on a one-member replica set of 8.2.11), the live `mongod` fixture where the
answer is an order that only mongod's own run can give.
"""

from __future__ import annotations

import base64
import datetime
import pathlib
import subprocess
from collections.abc import Iterator
from typing import Any

import pytest
from bson import Int64

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, mongo_answer

pymongo = pytest.importorskip("pymongo")


def _fresh(client: Any, name: str) -> Any:
    client.drop_database(name)
    return client[name]


def _seed(client: Any, name: str, docs: list, indexes: list, after: list = ()) -> Any:
    """A database `name` holding `c`: the documents, then the indexes, then
    the commands in `after`, as the probes seed it."""
    db = _fresh(client, name)
    db.c.insert_many([dict(d) for d in docs])
    if indexes:
        db.command("createIndexes", "c", indexes=indexes)
    for command in after:
        db.command(command)
    return db


def _ids(db: Any, command: dict) -> tuple:
    """A read's `_id`s in order, or the error."""

    def run() -> list:
        reply = db.command(command)
        return [d.get("_id") for d in reply["cursor"]["firstBatch"]]

    return mongo_answer(run)


def _index(key: dict, **options: Any) -> dict:
    name = options.pop("name", "_".join(f"{k}_{v}" for k, v in key.items()))
    return {"key": key, "name": name, **options}


# ===================================================== 7.083 users and roles


@pytest.mark.backlog("RM-029")
def test_roles_info_lists_roles_in_creation_order(rust_mongo: Any) -> None:
    """mongod 8.2.11: `rolesInfo: 1` lists stored roles in the order they
    were created, not by name."""
    db = _fresh(rust_mongo, "rm029")
    db.command("dropAllRolesFromDatabase")
    for name in ["zeta", "alpha", "mid"]:
        db.command("createRole", name, privileges=[], roles=[])
    got = mongo_answer(lambda: [r["role"] for r in db.command("rolesInfo", 1)["roles"]])
    assert got == ("ok", ["zeta", "alpha", "mid"])


# ======================================================== 7.082 diagnostics


@pytest.mark.backlog_fixed("RM-033")
def test_validate_system_views(rust_mongo: Any) -> None:
    """mongod 8.2.11 validates `system.views` as the collection it is."""
    db = _fresh(rust_mongo, "rm033")
    db.create_collection("src")
    db.command("create", "v", viewOn="src", pipeline=[])

    def run() -> dict:
        r = db.command("validate", "system.views")
        return {k: r.get(k) for k in ("ns", "valid", "nrecords", "nIndexes", "errors")}

    got = mongo_answer(run)
    assert got == (
        "ok",
        {"ns": "rm033.system.views", "valid": True, "nrecords": 1, "nIndexes": 1, "errors": []},
    )


@pytest.mark.backlog("RM-033")
def test_background_validate_of_an_uncheckpointed_collection(rust_mongo: Any) -> None:
    """mongod 8.2.11, one-member replica set: a background validation of a
    collection no checkpoint holds yet is 26 NamespaceNotFound."""
    db = _fresh(rust_mongo, "rm033b")
    db.create_collection("c")
    db.c.insert_one({})
    got = mongo_answer(lambda: db.command("validate", "c", background=True)["ok"])
    assert got == ("err", 26, "Collection 'rm033b.c' does not exist to validate.")


@pytest.mark.backlog("RM-034")
def test_server_status_sections(rust_mongo: Any) -> None:
    """mongod 8.2.11 (standalone and replica set) reports these sections."""
    want = {
        "asserts",
        "mem",
        "globalLock",
        "transactions",
        "wiredTiger",
        "locks",
        "opLatencies",
        "extra_info",
        "logicalSessionRecordCache",
        "security",
    }
    got = set(rust_mongo.admin.command("serverStatus"))
    assert want - got == set()


@pytest.mark.backlog_fixed("RM-034")
def test_server_status_metrics(rust_mongo: Any) -> None:
    """mongod 8.2.11's `metrics` holds `document`, `operation`, `query`,
    `commands` beside `cursor` and `ttl`."""
    got = set(rust_mongo.admin.command("serverStatus").get("metrics", {}))
    assert {"document", "operation", "query", "commands"} - got == set()


@pytest.mark.backlog_fixed("RM-035")
def test_set_parameter_at_runtime(rust_mongo: Any) -> None:
    """mongod 8.2.11 changes `cursorTimeoutMillis` and
    `transactionLifetimeLimitSeconds` while it runs (`was` is the old value)."""
    a = mongo_answer(
        lambda: rust_mongo.admin.command("setParameter", 1, cursorTimeoutMillis=600000)["was"]
    )
    b = mongo_answer(
        lambda: rust_mongo.admin.command("setParameter", 1, transactionLifetimeLimitSeconds=60)[
            "was"
        ]
    )
    assert (a, b) == (("ok", 600000), ("ok", 60))


@pytest.mark.backlog_fixed("RM-035")
def test_notablescan_parameter(rust_mongo: Any) -> None:
    """mongod 8.2.11 has `notablescan` (false by default)."""
    got = mongo_answer(
        lambda: rust_mongo.admin.command("getParameter", 1, notablescan=1)["notablescan"]
    )
    assert got == ("ok", False)


@pytest.mark.backlog_fixed("RM-036")
def test_replica_set_commands(rust_mongo: Any) -> None:
    """mongod 8.2.11, an initiated one-member replica set: these run (or, for
    `replSetInitiate`, say the set is already initiated)."""
    admin = rust_mongo.admin
    got = [
        mongo_answer(lambda: admin.command("replSetInitiate")["ok"])[:2],
        mongo_answer(lambda: admin.command("appendOplogNote", data={"x": 1})["ok"]),
        mongo_answer(lambda: admin.command("replSetResizeOplog", size=990.0)["ok"]),
    ]
    assert got == [("err", 23), ("ok", 1.0), ("ok", 1.0)]


@pytest.mark.backlog_fixed("RM-036")
def test_replica_set_internal_collections(rust_mongo: Any) -> None:
    """mongod 8.2.11, one-member replica set: `local` lists the oplog and the
    replica-set configuration, and `replSetGetStatus` has
    `electionCandidateMetrics`."""
    local = set(rust_mongo.local.list_collection_names())
    status = rust_mongo.admin.command("replSetGetStatus")
    assert (
        {"oplog.rs", "system.replset"} - local,
        "electionCandidateMetrics" in status,
    ) == (set(), True)


@pytest.mark.backlog_fixed("RM-037")
def test_commands_not_there(rust_mongo: Any) -> None:
    """mongod 8.2.11 runs `features`, `lockInfo`, `getClusterParameter` and
    the plan-cache commands."""
    admin = rust_mongo.admin
    db = rust_mongo["rm037"]
    db.c.insert_one({})
    got = [
        mongo_answer(lambda: admin.command("features")["ok"]),
        mongo_answer(lambda: admin.command("lockInfo")["ok"]),
        mongo_answer(lambda: admin.command("getClusterParameter", "*")["ok"]),
        mongo_answer(lambda: db.command("planCacheClear", "c")["ok"]),
        mongo_answer(lambda: db.command("planCacheListFilters", "c")["ok"]),
    ]
    assert got == [("ok", 1.0)] * 5


@pytest.mark.backlog_fixed("RM-037")
def test_admin_only_command_parses_its_argument_first(rust_mongo: Any) -> None:
    """mongod 8.2.11 parses `getClusterParameter`'s argument before it
    refuses a database other than `admin`."""
    got = mongo_answer(lambda: rust_mongo["rm037"].command("getClusterParameter", 5))
    assert got == (
        "err",
        14,
        "BSON field 'getClusterParameter.getClusterParameter' is the wrong type 'int', "
        "expected types '[string]'",
    )


@pytest.mark.backlog_fixed("RM-038")
def test_profile_filter_is_stored(rust_mongo: Any) -> None:
    """mongod 8.2.11 stores a profile `filter` and reports it, normalised."""
    db = _fresh(rust_mongo, "rm038")

    def run() -> Any:
        db.command("profile", 0, filter={"op": "query"})
        return db.command("profile", -1).get("filter")

    assert mongo_answer(run) == ("ok", {"op": {"$eq": "query"}})


@pytest.mark.backlog_fixed("RM-038")
def test_profile_level_creates_system_profile(rust_mongo: Any) -> None:
    """mongod 8.2.11: setting level 1 or 2 creates `system.profile` at once,
    capped."""
    got = []
    for level in (1, 2):
        db = _fresh(rust_mongo, f"rm038_{level}")
        db.command("profile", level, slowms=0)
        has = "system.profile" in db.list_collection_names()
        db.command("profile", 0)
        capped = mongo_answer(lambda db=db: db.command("collStats", "system.profile").get("capped"))
        got.append((has, capped))
    assert got == [(True, ("ok", True))] * 2


@pytest.mark.backlog_fixed("RM-039")
def test_hello_names_compressors(rust_mongo: Any) -> None:
    """mongod 8.2.11 answers a `compression` list with the ones it speaks."""
    reply = rust_mongo.admin.command("hello", compression=["zlib", "snappy", "zstd"])
    assert reply.get("compression") == ["zlib", "snappy", "zstd"]


@pytest.mark.backlog_fixed("RM-040")
def test_host_info_system_fields(rust_mongo: Any) -> None:
    """mongod 8.2.11 (macOS arm64): twelve `system` fields, a real memory
    size."""
    system = rust_mongo.admin.command("hostInfo")["system"]
    want = {
        "cpuAddrSize",
        "cpuArch",
        "currentTime",
        "hostname",
        "memLimitMB",
        "memSizeMB",
        "numCores",
        "numCoresAvailableToProcess",
        "numCpuSockets",
        "numNumaNodes",
        "numPhysicalCores",
        "numaEnabled",
    }
    assert (want - set(system), system.get("memSizeMB", 0) > 0) == (set(), True)


@pytest.mark.backlog("RM-040")
def test_coll_stats_wiredtiger_and_index_details(rust_mongo: Any) -> None:
    """mongod 8.2.11's `collStats` has `wiredTiger` and `indexDetails`."""
    db = _fresh(rust_mongo, "rm040cs")
    db.c.insert_one({"a": 1})
    stats = db.command("collStats", "c")
    assert ("wiredTiger" in stats, "indexDetails" in stats) == (True, True)


@pytest.mark.backlog_fixed("RM-040")
def test_index_stats_counts_use(rust_mongo: Any) -> None:
    """mongod 8.2.11: two finds through `a_1` count two accesses."""
    db = _fresh(rust_mongo, "rm040ix")
    db.c.insert_one({"a": 1})
    db.c.create_index("a")
    list(db.c.find({"a": 1}))
    list(db.c.find({"a": 1}))
    got = sorted((s["name"], s["accesses"]["ops"]) for s in db.c.aggregate([{"$indexStats": {}}]))
    assert got == [("_id_", 0), ("a_1", 2)]


@pytest.mark.backlog_fixed("RM-040")
def test_db_stats_counts_view_definitions(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts `system.views` in `dataSize` and `avgObjSize`."""

    def run(client: Any) -> tuple:
        db = _fresh(client, "rm040ds")
        db.c.insert_many([{"_id": 1, "a": "x" * 100}, {"_id": 2, "a": 1}])
        db.command("create", "v", viewOn="c", pipeline=[{"$match": {"a": {"$gt": 0}}}] * 5)
        s = db.command("dbStats")
        return (s["objects"], s["dataSize"], s["avgObjSize"])

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog("RM-040")
def test_list_databases_lists_admin_and_config(rust_mongo: Any) -> None:
    """mongod 8.2.11 lists `admin`, `config` and `local`."""
    names = {d["name"] for d in rust_mongo.admin.command("listDatabases")["databases"]}
    assert {"admin", "config", "local"} - names == set()


@pytest.mark.backlog_fixed("RM-040")
def test_list_catalog_stage(rust_mongo: Any) -> None:
    """mongod 8.2.11 has `$listCatalog`."""
    got = mongo_answer(lambda: len(list(rust_mongo.admin.aggregate([{"$listCatalog": {}}]))) > 0)
    assert got == ("ok", True)


@pytest.mark.backlog_fixed("RM-040")
def test_x509_without_a_certificate(rust_mongo: Any) -> None:
    """mongod 8.2.11 says only "Authentication failed." """
    got = mongo_answer(
        lambda: rust_mongo.admin.command("authenticate", mechanism="MONGODB-X509", user="CN=x")
    )
    assert got == ("err", 18, "Authentication failed.")


@pytest.mark.backlog_fixed("RM-040")
def test_sasl_start_base64_payload(rust_mongo: Any) -> None:
    """mongod 8.2.11 decodes a base64 string `saslStart` payload and goes on
    with the conversation."""
    db = rust_mongo["rm040u"]
    db.command("dropAllUsersFromDatabase")
    db.command("createUser", "bob", pwd="pw", roles=[], mechanisms=["SCRAM-SHA-256"])
    payload = base64.b64encode(b"n,,n=bob,r=abcdefghijklmnopqrstuvwx").decode()

    def run() -> tuple:
        r = db.command("saslStart", mechanism="SCRAM-SHA-256", payload=payload, autoAuthorize=1)
        return (r["ok"], r["done"], type(r["payload"]).__name__)

    assert mongo_answer(run) == ("ok", (1.0, False, "str"))


@pytest.mark.backlog("RM-143")
def test_features_reports_the_script_engine(rust_mongo: Any) -> None:
    """mongod 8.2.11's `features` has `js: {utf8: true}`: it has a script
    engine."""
    assert rust_mongo.admin.command("features").get("js") == {"utf8": True}


@pytest.mark.backlog("RM-143")
def test_features_oid_reset(rust_mongo: Any) -> None:
    """mongod 8.2.11 answers `oidReset` with the old machine value."""
    got = mongo_answer(
        lambda: "oidMachineOld" in rust_mongo.admin.command("features", oidReset=True)
    )
    assert got == ("ok", True)


@pytest.mark.backlog("RM-143")
def test_plan_cache_set_filter(rust_mongo: Any) -> None:
    """mongod 8.2.11 sets an index filter and lists it."""
    db = _fresh(rust_mongo, "rm_pcsf")
    db.c.insert_one({"a": 1})
    db.c.create_index("a")

    def run() -> int:
        db.command("planCacheSetFilter", "c", query={"a": 1}, indexes=[{"a": 1}])
        return len(db.command("planCacheListFilters", "c")["filters"])

    assert mongo_answer(run) == ("ok", 1)


@pytest.mark.backlog("RM-143")
def test_resize_oplog_min_retention_hours(rust_mongo: Any) -> None:
    """mongod 8.2.11 (one-member replica set) takes a `minRetentionHours`."""
    got = mongo_answer(
        lambda: rust_mongo.admin.command("replSetResizeOplog", 1, minRetentionHours=1.5)["ok"]
    )
    assert got == ("ok", 1.0)


@pytest.mark.backlog("RM-143")
def test_every_cluster_parameter(rust_mongo: Any) -> None:
    """mongod 8.2.11 (one-member replica set) has fifteen cluster
    parameters, `fleCompactionOptions` and `internalSearchOptions` among
    them."""
    got = mongo_answer(
        lambda: len(rust_mongo.admin.command("getClusterParameter", "*")["clusterParameters"])
    )
    assert got == ("ok", 15)


# ================================================================= 7.081 geo

POINTS = [(10, 10), (-50, 20), (100, -30), (0.5, 0.5), (-120, 45), (30, -60), (170, 5)]


@pytest.mark.backlog_fixed("RM-041")
def test_geo_within_in_2dsphere_scan_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod returns a `$geoWithin` over a `2dsphere` index in S2 cell
    order."""
    docs = [
        {"_id": i, "loc": {"type": "Point", "coordinates": list(p)}} for i, p in enumerate(POINTS)
    ]
    command = {"find": "c", "filter": {"loc": {"$geoWithin": {"$centerSphere": [[0, 0], 3.0]}}}}

    def run(client: Any) -> tuple:
        return _ids(_seed(client, "rm041s", docs, [_index({"loc": "2dsphere"})]), command)

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-041")
def test_geo_within_in_2d_scan_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod returns a `$geoWithin` over a `2d` index in geohash order."""
    docs = [{"_id": i, "loc": list(p)} for i, p in enumerate(POINTS)]
    command = {"find": "c", "filter": {"loc": {"$geoWithin": {"$box": [[-180, -90], [180, 90]]}}}}

    def run(client: Any) -> tuple:
        return _ids(_seed(client, "rm041f", docs, [_index({"loc": "2d"})]), command)

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-041")
def test_near_ties_in_scan_order(rust_mongo: Any, mongod: Any) -> None:
    """Four points at one distance from a `$near`: mongod returns them in its
    scan's order."""
    docs = [{"_id": 0, "loc": [1, 0]}, {"_id": 1, "loc": [0, 1]}, {"_id": 2, "loc": [-1, 0]}]
    docs.append({"_id": 3, "loc": [0, -1]})
    command = {"find": "c", "filter": {"loc": {"$near": [0, 0]}}}

    def run(client: Any) -> tuple:
        return _ids(_seed(client, "rm041n", docs, [_index({"loc": "2d"})]), command)

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-042")
def test_2d_index_options_refused(rust_mongo: Any) -> None:
    """mongod 8.2.11 refuses `bits` over 32 and `min` not below `max` (72)."""
    db = _fresh(rust_mongo, "rm042")
    got = [
        mongo_answer(lambda: db.x.create_index([("loc", "2d")], bits=40, name="b40")),
        mongo_answer(lambda: db.y.create_index([("loc", "2d")], min=5, max=1, name="mm")),
    ]
    assert got == [
        ("err", 72, "bits for hash must be > 0 and <= 32, but 40 bits were specified"),
        (
            "err",
            72,
            "region for hash must be valid and have positive area, but [5, 1] was specified",
        ),
    ]


@pytest.mark.backlog_fixed("RM-042")
def test_2d_index_options_listed(rust_mongo: Any) -> None:
    """mongod 8.2.11 lists `bits` as an int and `min` / `max` as doubles,
    whatever type they were given as."""
    db = _fresh(rust_mongo, "rm042l")
    db.c.create_index([("loc", "2d")], name="l", bits=Int64(20), min=Int64(-10), max=Int64(10))
    spec = next(x for x in db.c.list_indexes() if x["name"] == "l")
    got = {k: (spec.get(k), type(spec.get(k)).__name__) for k in ("bits", "min", "max")}
    assert got == {"bits": (20, "int"), "min": (-10.0, "float"), "max": (10.0, "float")}


@pytest.mark.backlog_fixed("RM-043")
def test_near_centre_outside_2d_range(rust_mongo: Any) -> None:
    """mongod 8.2.11: a `$near` centre outside the index's range is 16433."""
    docs = [{"_id": 0, "loc": [1, 1]}, {"_id": 1, "loc": [5, 5]}, {"_id": 2, "loc": [10, 10]}]
    db = _seed(rust_mongo, "rm043", docs, [_index({"loc": "2d"}, min=0, max=10)])
    got = _ids(db, {"find": "c", "filter": {"loc": {"$near": [20, 20]}}})
    assert got[:2] == ("err", 16433)


def _hinted_geo(client: Any) -> Any:
    docs = [
        {"_id": 0, "loc": {"type": "Point", "coordinates": [100, 10]}},
        {"_id": 1, "x": 1},
        {"_id": 2, "loc": {"type": "Point", "coordinates": [-100, -10]}},
        {"_id": 3, "loc": {"type": "Point", "coordinates": [1, 1]}},
    ]
    return _seed(
        client, "rm044", docs, [_index({"loc": "2dsphere"}, name="g"), _index({"x": 1}, name="x1")]
    )


@pytest.mark.backlog_fixed("RM-044")
def test_hinted_2dsphere_index_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod returns what a hinted `2dsphere` index holds, in its order."""
    command = {"find": "c", "filter": {}, "hint": "g"}
    assert _ids(_hinted_geo(rust_mongo), command) == _ids(_hinted_geo(mongod), command)


@pytest.mark.backlog_fixed("RM-044")
def test_near_hinted_to_another_index(rust_mongo: Any) -> None:
    """mongod 8.2.11 refuses a `$near` hinted to a non-geo index (291)."""
    near = {"loc": {"$near": {"$geometry": {"type": "Point", "coordinates": [0, 0]}}}}
    got = _ids(_hinted_geo(rust_mongo), {"find": "c", "filter": near, "hint": "x1"})
    assert got[:2] == ("err", 291)


@pytest.mark.backlog_fixed("RM-045")
def test_2dsphere_index_version_not_an_integer(rust_mongo: Any) -> None:
    """mongod 8.2.11 refuses `2dsphereIndexVersion: 2.5` (2)."""
    db = _fresh(rust_mongo, "rm045")
    got = mongo_answer(
        lambda: db.a.create_index([("loc", "2dsphere")], **{"2dsphereIndexVersion": 2.5})
    )
    assert got == (
        "err",
        2,
        'Expected field "2dsphereIndexVersion" to have a value exactly representable as a '
        "64-bit integer, but found 2dsphereIndexVersion: 2.5",
    )


@pytest.mark.backlog_fixed("RM-045")
def test_two_indexes_one_key_in_one_create_indexes(rust_mongo: Any) -> None:
    """mongod 8.2.11: two specs with one key in one `createIndexes` is 85."""
    db = _fresh(rust_mongo, "rm045b")
    indexes = [
        {"key": {"loc": "2dsphere"}, "name": "one"},
        {"key": {"loc": "2dsphere"}, "name": "two"},
    ]
    got = mongo_answer(lambda: db.command("createIndexes", "b", indexes=indexes))
    assert got == ("err", 85, "Index already exists with a different name: one")


@pytest.mark.backlog_fixed("RM-045")
def test_geo_near_in_lookup_wrapper(rust_mongo: Any) -> None:
    """mongod 8.2.11 wraps the `$geoNear` refusal inside `$lookup` and
    `$unionWith` with the executor's prefix."""
    db = _fresh(rust_mongo, "rm045c")
    db.o.insert_one({"_id": 1})
    db.g.insert_one({"_id": 1, "loc": {"type": "Point", "coordinates": [0, 0]}})
    gn = {
        "$geoNear": {
            "near": {"type": "Point", "coordinates": [0, 0]},
            "distanceField": "d",
            "spherical": True,
        }
    }
    msg = (
        "Executor error during aggregate command on namespace: rm045c.o :: caused by :: "
        "$geoNear requires a 2d or 2dsphere index, but none were found"
    )
    got = [
        mongo_answer(
            lambda: list(db.o.aggregate([{"$lookup": {"from": "g", "pipeline": [gn], "as": "x"}}]))
        ),
        mongo_answer(
            lambda: list(db.o.aggregate([{"$unionWith": {"coll": "g", "pipeline": [gn]}}]))
        ),
    ]
    assert got == [("err", 27, msg)] * 2


SPHERE = [
    {"_id": 4, "loc": {"type": "Point", "coordinates": [1, 1]}},
    {"_id": 1, "loc": {"type": "Point", "coordinates": [0, 0]}},
    {"_id": 9, "loc": {"type": "Point", "coordinates": [10, 10]}},
    {"_id": 2, "loc": {"type": "Point", "coordinates": [2.5, 2.5]}},
    {"_id": 7, "loc": {"type": "Point", "coordinates": [-73.97, 40.77]}},
    {"_id": 3, "loc": {"type": "Point", "coordinates": [-73.88, 40.78]}},
    {"_id": 12, "loc": {"type": "Point", "coordinates": [179.9, 0]}},
    {"_id": 5, "loc": {"type": "Point", "coordinates": [-179.9, 0]}},
    {"_id": 8, "loc": {"type": "Point", "coordinates": [0, 89.9]}},
    {"_id": 6, "loc": {"type": "LineString", "coordinates": [[0, 0], [2, 2]]}},
    {"_id": 13, "loc": [1.5, 1.5]},
    {"_id": 14, "loc": {"lng": 2, "lat": 2}},
    {"_id": 15},
    {"_id": 17, "loc": {"type": "MultiPoint", "coordinates": [[1, 1], [30, 30]]}},
]


@pytest.mark.backlog_fixed("RM-046")
def test_center_sphere_over_pi_with_2dsphere_index(rust_mongo: Any) -> None:
    """mongod 8.2.11, documents of `tools/probes/geo_queries.py`: a
    `$centerSphere` of radius 4 over a `2dsphere` index returns every
    document with a location (measured with the index built before and after
    the documents; the far points 5, 8 and 12 are in both)."""
    db = _seed(rust_mongo, "rm046", SPHERE, [_index({"loc": "2dsphere"})])
    command = {"find": "c", "filter": {"loc": {"$geoWithin": {"$centerSphere": [[0, 0], 4]}}}}
    ok, ids = _ids(db, command)
    assert (ok, sorted(ids)) == ("ok", [1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13, 14, 17])


@pytest.mark.backlog_fixed("RM-047")
def test_center_sphere_and_a_line_bulging_out(rust_mongo: Any) -> None:
    """mongod 8.2.11: a line with both ends inside a `$centerSphere` and its
    great-circle arc outside it is not within it."""
    db = _fresh(rust_mongo, "rm047l")
    db.c.insert_many(
        [
            {"_id": 1, "loc": {"type": "LineString", "coordinates": [[-80, 20], [80, 20]]}},
            {"_id": 2, "loc": {"type": "Point", "coordinates": [0, 20]}},
        ]
    )
    command = {"find": "c", "filter": {"loc": {"$geoWithin": {"$centerSphere": [[0, -90], 2.0]}}}}
    assert _ids(db, command) == ("ok", [2])


@pytest.mark.backlog_fixed("RM-047")
def test_strictwinding_crs(rust_mongo: Any) -> None:
    """mongod 8.2.11: under the `strictwinding` crs a clockwise ring is the
    big polygon outside it."""
    db = _fresh(rust_mongo, "rm047s")
    db.c.insert_many(
        [
            {"_id": 1, "loc": {"type": "Point", "coordinates": [0, 0]}},
            {"_id": 2, "loc": {"type": "Point", "coordinates": [100, 50]}},
        ]
    )
    ring = [[-10, -10], [-10, 20], [20, 20], [20, -10], [-10, -10]]
    crs = {"type": "name", "properties": {"name": "urn:x-mongodb:crs:strictwinding:EPSG:4326"}}
    geometry = {"type": "Polygon", "coordinates": [ring], "crs": crs}
    command = {"find": "c", "filter": {"loc": {"$geoWithin": {"$geometry": geometry}}}}
    assert _ids(db, command) == ("ok", [2])


# ======================================================== 7.080 find order

DAY = datetime.datetime(2020, 1, 2)
A_VALUES = [
    (11, 5),
    (4, 3),
    (17, "b"),
    (2, 5),
    (9, None),
    (14, ...),
    (6, 3.0),
    (20, [1, 9]),
    (1, "a"),
    (13, 7),
    (8, True),
    (16, {"x": 1}),
    (3, 5),
    (19, Int64(3)),
    (7, [5, 3]),
    (12, 1),
    (5, DAY),
    (18, "B"),
    (10, []),
    (15, 9.5),
]


def _mixed() -> list:
    docs = []
    for place, (id_, a) in enumerate(A_VALUES):
        doc: dict = {"_id": id_}
        if a is not ...:
            doc["a"] = a
        doc["b"] = place % 4
        doc["c"] = (place * 7) % 5
        docs.append(doc)
    return docs


MIXED = _mixed()
MOVED = [
    {"update": "c", "updates": [{"q": {"_id": 2}, "u": {"$set": {"pad": "x" * 4000}}}]},
    {"delete": "c", "deletes": [{"q": {"_id": {"$in": [11, 13]}}, "limit": 0}]},
    {"insert": "c", "documents": [{"_id": 13, "a": 5, "b": 0, "c": 0}, {"_id": 0, "a": 5, "b": 1}]},
    {"update": "c", "updates": [{"q": {"_id": 4}, "u": {"_id": 4, "a": 5, "b": 9}}]},
    {"update": "c", "updates": [{"q": {"_id": 12}, "u": {"$set": {"a": 5}}}]},
    {"update": "c", "updates": [{"q": {"_id": 99}, "u": {"$set": {"a": 5}}, "upsert": True}]},
    {"update": "c", "updates": [{"q": {"b": 3}, "u": {"$inc": {"c": 1}}, "multi": True}]},
]


def _same_order(rust: Any, ref: Any, name: str, indexes: list, command: dict, **kw: Any) -> None:
    docs = kw.pop("docs", MIXED)
    after = kw.pop("after", [])
    got = _ids(_seed(rust, name, docs, indexes, after), command)
    want = _ids(_seed(ref, name, docs, indexes, after), command)
    assert got == want


@pytest.mark.backlog("RM-049")
def test_ties_under_a_sort_with_a_limit(rust_mongo: Any, mongod: Any) -> None:
    """`{a: 5}, sort {a: -1}, skip 1, limit 2`, no index: mongod's top-k heap
    leaves the tied documents in its own order."""
    command = {"find": "c", "filter": {"a": 5}, "sort": {"a": -1}, "skip": 1, "limit": 2}
    _same_order(rust_mongo, mongod, "rm049", [], command)


@pytest.mark.backlog("RM-049")
def test_ties_under_an_aggregate_sort_with_a_limit(rust_mongo: Any, mongod: Any) -> None:
    """`$sort {b: 1}, $limit 3`, no index: the heap's order of the ties."""
    command = {
        "aggregate": "c",
        "pipeline": [{"$sort": {"b": 1}}, {"$limit": 3}],
        "cursor": {},
    }
    _same_order(rust_mongo, mongod, "rm049a", [], command)


@pytest.mark.backlog_fixed("RM-050")
def test_ties_between_merged_scans(rust_mongo: Any, mongod: Any) -> None:
    """`{a: {$in: [5, 3]}}, sort {b: 1}` over `{a: 1, b: 1}`: mongod merges a
    scan per value through a priority queue."""
    command = {"find": "c", "filter": {"a": {"$in": [5, 3]}}, "sort": {"b": 1}}
    _same_order(rust_mongo, mongod, "rm050", [_index({"a": 1, "b": 1})], command)


@pytest.mark.backlog_fixed("RM-051")
def test_unknown_hint_on_aggregate(rust_mongo: Any, mongod: Any) -> None:
    """An `aggregate` hinted to an index that does not exist is refused with
    mongod's code. The message is this server's own: mongod prints its query
    tree there, and not reproducing it is a scope decision (Joe, 2026-10-16;
    `tests/test_rust_hint_error_message.py`)."""
    command = {"aggregate": "c", "pipeline": [{"$match": {"a": 5}}], "cursor": {}, "hint": "a_1"}
    answers = []
    for client in (rust_mongo, mongod):
        db = client["rm051"]
        db.drop_collection("c")
        db.c.insert_one({"a": 5})  # on a missing collection mongod ignores the hint
        answers.append(mongo_answer(lambda db=db: db.command(command)))
    got, want = answers
    assert got[:2] == want[:2] == ("err", 2)


THREE = [_index({"a": 1}), _index({"b": 1}), _index({"a": 1, "b": 1})]


@pytest.mark.backlog_fixed("RM-052")
def test_which_predicate_index_is_read(rust_mongo: Any, mongod: Any) -> None:
    """`{a: {$gt: 0}, b: 1}` with `a_1`, `b_1` and `a_1_b_1`: mongod reads
    `b_1`, and returns the documents in its order."""
    command = {"find": "c", "filter": {"a": {"$gt": 0}, "b": 1}}
    _same_order(rust_mongo, mongod, "rm052", THREE, command)


@pytest.mark.backlog_fixed("RM-053")
def test_match_after_add_fields_reads_its_index(rust_mongo: Any, mongod: Any) -> None:
    """mongod moves a `$match` that reads no field `$addFields` writes ahead
    of it, and reads `b_1` for it."""
    command = {
        "aggregate": "c",
        "pipeline": [{"$match": {}}, {"$addFields": {"z": 1}}, {"$match": {"b": {"$gte": 1}}}],
        "cursor": {},
    }
    _same_order(rust_mongo, mongod, "rm053", [_index({"a": 1}), _index({"b": 1})], command)


WORDS = [
    {"_id": place, "w": w, "g": place % 2}
    for place, w in enumerate(["b", "A", "a", "B", "c", "á", "C", "ab", "Ab", "aB", "", None])
]
CI = {"locale": "en", "strength": 2}


@pytest.mark.backlog_fixed("RM-054")
def test_regex_under_a_collation_with_two_indexes(rust_mongo: Any, mongod: Any) -> None:
    """`{w: {$regex: "^a"}}, sort {w: 1}` under a collation, with a plain and
    a collated index on `w`."""
    indexes = [_index({"w": 1}), _index({"w": 1}, collation=CI, name="w_ci")]
    command = {"find": "c", "filter": {"w": {"$regex": "^a"}}, "sort": {"w": 1}, "collation": CI}
    _same_order(rust_mongo, mongod, "rm054", indexes, command, docs=WORDS)


@pytest.mark.backlog_fixed("RM-054")
def test_in_sort_after_writes(rust_mongo: Any, mongod: Any) -> None:
    """`{a: {$in: [5, 3]}}, sort {a: -1}` over `a_1` after writes that move
    documents: the same merge as RM-050."""
    command = {
        "find": "c",
        "filter": {"a": {"$in": [5, 3]}},
        "projection": {"pad": 0},
        "sort": {"a": -1},
    }
    _same_order(rust_mongo, mongod, "rm054w", [_index({"a": 1})], command, after=MOVED)


# ------------------------------------------------------------ RM-055 restart


def _start_rust(store: pathlib.Path) -> tuple[subprocess.Popen, Any]:
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    m = _RS_BANNER.search(proc.stdout.readline())
    assert m, "secantusd-rs did not start"
    client = pymongo.MongoClient(
        host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
    )
    return proc, client


def _stop_rust(proc: subprocess.Popen, client: Any) -> None:
    client.close()
    proc.terminate()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()


@pytest.fixture
def restarted(tmp_path: pathlib.Path) -> Iterator[Any]:
    """A `secantusd-rs` whose `rm055.c` once held an array under `a_1` and
    no longer does, restarted after that."""
    proc, client = _start_rust(tmp_path)
    try:
        db = client.rm055
        db.c.create_index("a")
        db.c.insert_one({"_id": 1, "a": [1, 2]})
        db.c.update_one({"_id": 1}, {"$set": {"a": 5}})
    finally:
        _stop_rust(proc, client)
    proc, client = _start_rust(tmp_path)
    try:
        yield client
    finally:
        _stop_rust(proc, client)


def _ixscan(plan: dict) -> dict:
    while plan.get("stage") != "IXSCAN":
        plan = plan["inputStage"]
    return plan


@pytest.mark.backlog("RM-055")
def test_multikey_paths_survive_a_restart(restarted: Any) -> None:
    """mongod 8.2.11: once `a_1` has held an array it stays multikey, with
    `multiKeyPaths: {a: ["a"]}`, though no document holds an array now."""
    plan = restarted.rm055.command(
        "explain", {"find": "c", "filter": {"a": 5}, "hint": "a_1"}, verbosity="queryPlanner"
    )["queryPlanner"]["winningPlan"]
    scan = _ixscan(plan)
    assert (scan.get("isMultiKey"), scan.get("multiKeyPaths")) == (True, {"a": ["a"]})
