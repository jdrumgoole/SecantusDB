"""The keys a DISTINCT_SCAN examines, against mongod 8.2 (RM-377).

mongod's DISTINCT_SCAN has no end key: every unit of its work seeks, and
it counts the key each seek lands on before it checks it against the
bounds (`DistinctScan::doWork`). So a scan whose bounds end before the
index does examines the key past them, and one that runs to the end of
the index does not. The count is the profile entry's and the slow-query
line's `keysExamined`, its `execStats`', and `explain`'s.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest

from tests.backlog.opdebug import logged

from .conftest import same


def _distinct_keys(stats: Any) -> Any:
    while isinstance(stats, dict):
        if stats.get("stage") == "DISTINCT_SCAN":
            return stats.get("keysExamined")
        stats = stats.get("inputStage")
    return None


def _seed(index: Any, values: Callable[[int], int]) -> Callable[[Any], None]:
    def setup(db: Any) -> None:
        db.c.insert_many([{"_id": n, "x": values(n), "z": n % 2} for n in range(12)])
        db.c.create_index(index)

    return setup


_CASES = {
    # The key past the bounds: 4.
    "below": (_seed("x", lambda n: n), {"x": {"$lt": 4}}),
    # The bounds run to the end of the index: no key past them.
    "to-the-end": (_seed("x", lambda n: n), {"x": {"$gte": 4}}),
    # A descending index, read forward: 11 down to 5, then 4.
    "descending": (_seed([("x", -1)], lambda n: n), {"x": {"$gte": 5}}),
    # Runs of equal keys: a seek past each run, then past the bounds.
    "runs": (_seed("x", lambda n: n % 4), {"x": {"$lte": 1}}),
}


def _distinct(client: Any, is_mongod: bool, case: str) -> dict:
    setup, query = _CASES[case]
    lines, entries = logged(
        client, is_mongod, setup, lambda d: d.c.distinct("x", query), prefix="dscan"
    )
    return {
        "line": [a.get("keysExamined") for _, a in lines],
        "entry": [e.get("keysExamined") for e in entries],
        "execStats": [_distinct_keys(e.get("execStats")) for e in entries],
    }


@pytest.mark.parametrize("case", sorted(_CASES))
def test_a_distinct_examines_mongods_keys(rust_mongo: Any, mongod: Any, case: str) -> None:
    same(_distinct(rust_mongo, False, case), _distinct(mongod, True, case))


def _explained(client: Any, case: str) -> Any:
    setup, query = _CASES[case]
    db = client["dscan_explain"]
    db.drop_collection("c")
    setup(db)
    e = db.command(
        "explain", {"distinct": "c", "key": "x", "query": query}, verbosity="executionStats"
    )
    client.drop_database(db.name)
    stats = e["executionStats"]
    return stats["totalKeysExamined"], _distinct_keys(stats["executionStages"])


@pytest.mark.parametrize("case", sorted(_CASES))
def test_explain_counts_the_keys_a_distinct_examines(
    rust_mongo: Any, mongod: Any, case: str
) -> None:
    same(_explained(rust_mongo, case), _explained(mongod, case))


def _grouped(client: Any, is_mongod: bool) -> dict:
    setup, _ = _CASES["below"]
    pipeline = [{"$match": {"x": {"$lt": 4}}}, {"$sort": {"x": 1}}, {"$group": {"_id": "$x"}}]
    lines, entries = logged(
        client, is_mongod, setup, lambda d: list(d.c.aggregate(pipeline)), prefix="dscan"
    )
    return {
        "line": [a.get("keysExamined") for _, a in lines],
        "entry": [e.get("keysExamined") for e in entries],
    }


def test_a_group_answered_from_a_distinct_scan_examines_mongods_keys(
    rust_mongo: Any, mongod: Any
) -> None:
    same(_grouped(rust_mongo, False), _grouped(mongod, True))
