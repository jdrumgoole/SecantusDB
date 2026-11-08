"""Divergences found while letting a trial run over collections of any size
(RM-342, 2026-11-08), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _explained(client: Any, filter: dict) -> dict:
    client.drop_database("rm342_found")
    col = client.rm342_found.c
    col.insert_many([{"_id": i, "a": i % 100, "c": i % 7} for i in range(2000)])
    col.create_index([("a", 1), ("c", 1)])
    e = col.database.command("explain", {"find": "c", "filter": filter}, verbosity="executionStats")
    return e["executionStats"]["executionStages"]


def _stages(stage: Any) -> list:
    out = []
    while isinstance(stage, dict):
        out.append(stage)
        stage = stage.get("inputStage")
    return out


@pytest.mark.backlog("RM-433")
def test_a_scan_of_two_in_lists_examines_mongods_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 scans `{a: {$in: [3, 60]}, c: {$in: [1, 5]}}` over
    `a_1_c_1` (2,000 documents, `a: i % 100`, `c: i % 7`) examining 16
    keys; this server's scan counts 18."""

    def keys(client: Any) -> list:
        f = {"a": {"$in": [3, 60]}, "c": {"$in": [1, 5]}}
        return [s.get("keysExamined") for s in _stages(_explained(client, f))]

    assert keys(mongod) == [None, 16]
    assert keys(rust_mongo) == keys(mongod)


@pytest.mark.backlog("RM-434")
def test_explain_gives_each_stage_its_work_accounting(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `executionStats` gives each stage of the winning
    plan its work accounting -- `works`, `advanced`, `needTime`,
    `needYield`, `isEOF`, and a FETCH's `alreadyHasObj`, an IXSCAN's
    `seeks`, `dupsTested` and `dupsDropped` -- for `{a: 3}` over `a_1_c_1`.
    This server's stages carry only their `nReturned` and examined counts."""

    def fields(client: Any) -> list:
        return [
            (s["stage"], sorted(k for k in s if k not in ("saveState", "restoreState")))
            for s in _stages(_explained(client, {"a": 3}))
        ]

    assert fields(rust_mongo) == fields(mongod)
