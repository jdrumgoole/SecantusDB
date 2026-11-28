"""RM-824: the winner of a multi-plan trial reports one `works` fewer than
mongod. Measured on mongod 8.2.11, 2026-11-28: on a plain collection of 2,000
documents with `a_1` and `b_1`, a `find` of `{a: 1, b: 1}` has winning stages
FETCH `works` 22 over IXSCAN `works` 21; this server (0.5.3-beta.199) says
21 over 21. The rejected plans' counters already match.
"""

from __future__ import annotations

from typing import Any

import pytest

DB = "rm_824"
CASES = [
    {"a": 1, "b": 1},
    {"a": {"$lt": 50}, "b": {"$lt": 5}},
    {"a": 3, "b": {"$gte": 0}},
]


def _works(client: Any) -> list:
    db = client[DB]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "a": i % 100, "b": (i // 20) % 100} for i in range(2000)])
    db.c.create_index("a")
    db.c.create_index("b")
    out = []
    for flt in CASES:
        stats = db.command({"explain": {"find": "c", "filter": flt}, "verbosity": "executionStats"})
        stage = stats["executionStats"]["executionStages"]
        rows = []
        while True:
            rows.append((stage["stage"], stage["works"], stage["advanced"], stage["needTime"]))
            if "inputStage" not in stage:
                break
            stage = stage["inputStage"]
        out.append(rows)
    return out


@pytest.mark.backlog("RM-824")
def test_the_winners_works_count_the_final_eof_run(rust_mongo: Any, mongod: Any) -> None:
    expected = _works(mongod)
    assert expected[0] == [("FETCH", 22, 0, 20), ("IXSCAN", 21, 20, 0)], expected
    assert _works(rust_mongo) == expected
