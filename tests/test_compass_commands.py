"""Pin the command surface MongoDB Compass issues, headlessly.

Compass itself is an Electron GUI and can't be CI-automated, but every
screen boils down to a known set of wire commands. These tests drive
those exact shapes via pymongo so a regression in any of them is caught
without the GUI: instance probes on connect, the databases/collections
tree, the schema tab's ``$sample``, the indexes tab's ``listIndexes`` +
``$indexStats``, the explain-plan tab's two verbosities, and the
performance tab's ``serverStatus`` / ``top`` / ``currentOp`` polls.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["shop"]["items"]
        coll.insert_many([{"_id": i, "n": i, "tag": f"t{i % 3}"} for i in range(50)])
        coll.create_index([("n", 1)])
        yield mc
    finally:
        mc.close()


# ---- connect-time instance probes -------------------------------------------


def test_instance_detail_probes(client: MongoClient) -> None:
    """The command volley Compass fires on connect to build its
    instance view. Each must return ok:1 with the keys Compass reads."""
    build_info = client.admin.command("buildInfo")
    assert "version" in build_info and "gitVersion" in build_info

    fcv = client.admin.command({"getParameter": 1, "featureCompatibilityVersion": 1})
    assert "featureCompatibilityVersion" in fcv

    conn_status = client.admin.command({"connectionStatus": 1, "showPrivileges": True})
    assert "authInfo" in conn_status

    host_info = client.admin.command("hostInfo")
    assert "system" in host_info and "os" in host_info

    cmd_line = client.admin.command("getCmdLineOpts")
    assert "argv" in cmd_line and "parsed" in cmd_line


def test_atlas_version_returns_command_not_found(client: MongoClient) -> None:
    """Compass probes ``atlasVersion`` to detect Atlas; on-prem mongod
    (and SecantusDB) must answer CommandNotFound (59), which Compass
    swallows — not a connection-killing error."""
    with pytest.raises(OperationFailure) as exc:
        client.admin.command("atlasVersion")
    assert exc.value.code == 59
    # Connection must survive the unknown command.
    assert client.admin.command("ping")["ok"] == 1.0


def test_databases_tree(client: MongoClient) -> None:
    """Sidebar tree: listDatabases → per-db dbStats → listCollections
    nameOnly."""
    dbs = client.admin.command("listDatabases")
    assert any(d["name"] == "shop" for d in dbs["databases"])
    assert "totalSize" in dbs

    stats = client["shop"].command("dbStats")
    for key in ("collections", "objects", "dataSize", "indexes", "indexSize"):
        assert key in stats, key

    colls = client["shop"].command({"listCollections": 1, "nameOnly": True})
    batch = colls["cursor"]["firstBatch"]
    assert {k: v for k, v in batch[0].items() if k in ("name", "type")} == {
        "name": "items",
        "type": "collection",
    }


# ---- collection screens ------------------------------------------------------


def test_collection_stats_via_coll_stats_stage(client: MongoClient) -> None:
    """The collection header's document/size figures come from the
    ``$collStats`` aggregation stage with ``storageStats``."""
    out = client["shop"].command(
        {
            "aggregate": "items",
            "pipeline": [{"$collStats": {"storageStats": {"scale": 1}}}],
            "cursor": {},
        }
    )
    doc = out["cursor"]["firstBatch"][0]
    assert doc["ns"] == "shop.items"
    storage = doc["storageStats"]
    assert storage["count"] == 50
    for key in ("size", "avgObjSize", "storageSize", "nindexes", "totalIndexSize"):
        assert key in storage, key


def test_schema_tab_sample(client: MongoClient) -> None:
    """Schema analysis samples the collection with ``$sample``."""
    out = client["shop"].command(
        {
            "aggregate": "items",
            "pipeline": [{"$sample": {"size": 10}}],
            "cursor": {},
        }
    )
    batch = out["cursor"]["firstBatch"]
    assert len(batch) == 10
    assert all(set(d) == {"_id", "n", "tag"} for d in batch)
    # Distinct docs — sampling must not repeat documents.
    assert len({d["_id"] for d in batch}) == 10


def test_indexes_tab(client: MongoClient) -> None:
    """Indexes tab joins ``listIndexes`` with ``$indexStats`` usage."""
    idx = client["shop"].command({"listIndexes": "items"})
    names = {i["name"] for i in idx["cursor"]["firstBatch"]}
    assert names == {"_id_", "n_1"}

    out = client["shop"].command(
        {"aggregate": "items", "pipeline": [{"$indexStats": {}}], "cursor": {}}
    )
    stats = {d["name"]: d for d in out["cursor"]["firstBatch"]}
    assert set(stats) == {"_id_", "n_1"}
    entry = stats["n_1"]
    assert entry["key"] == {"n": 1}
    assert "accesses" in entry and "ops" in entry["accesses"]


# ---- explain plan tab --------------------------------------------------------


def test_explain_plan_query_planner(client: MongoClient) -> None:
    out = client["shop"].command(
        {
            "explain": {"find": "items", "filter": {"n": {"$gt": 5}}},
            "verbosity": "queryPlanner",
        }
    )
    plan = out["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "FETCH"
    assert plan["inputStage"]["stage"] == "IXSCAN"
    assert plan["inputStage"]["indexName"] == "n_1"


def test_explain_plan_execution_stats(client: MongoClient) -> None:
    """Compass's explain tab requests executionStats verbosity and
    renders nReturned / totalDocsExamined."""
    out = client["shop"].command(
        {
            "explain": {"find": "items", "filter": {"n": {"$gt": 5}}},
            "verbosity": "executionStats",
        }
    )
    stats = out["executionStats"]
    assert stats["nReturned"] == 44
    assert "totalDocsExamined" in stats and "executionTimeMillis" in stats


def test_explain_aggregate_lifts_leading_match(client: MongoClient) -> None:
    """Aggregate-explain must report the same plan the real pipeline
    run uses: a leading $match on an indexed field is lifted into the
    initial fetch, so the winningPlan is IXSCAN, not COLLSCAN."""
    out = client["shop"].command(
        {
            "explain": {
                "aggregate": "items",
                "pipeline": [{"$match": {"n": {"$gt": 5}}}],
                "cursor": {},
            },
            "verbosity": "executionStats",
        }
    )
    plan = out["queryPlanner"]["winningPlan"]
    assert plan["inputStage"]["stage"] == "IXSCAN"
    assert plan["inputStage"]["indexName"] == "n_1"
    assert out["executionStats"]["nReturned"] == 44
    assert "stages" in out and "$cursor" in out["stages"][0]


# ---- performance tab ---------------------------------------------------------


def test_performance_tab_polls(client: MongoClient) -> None:
    """Performance tab polls serverStatus / top / currentOp on a timer;
    all three must stay mongod-shaped."""
    status = client.admin.command("serverStatus")
    for key in ("connections", "opcounters", "network", "mem", "uptime"):
        assert key in status, key

    top = client.admin.command("top")
    assert "shop.items" in top["totals"]

    ops = client.admin.command("currentOp")
    assert isinstance(ops["inprog"], list)

    log = client.admin.command({"getLog": "global"})
    assert "log" in log and "totalLinesWritten" in log
