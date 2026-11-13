"""What a contested read's `Slow query` line still says wrongly, found while
working RM-363 (2026-11-13): the plan it names, and how it chose it.

Each test runs one read two indexes could serve on both servers with every
operation logged (`tests/backlog/opdebug.py`) and compares what the line
says of its plan with mongod 8.2.11's.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged


def _contested(db: Any) -> None:
    """400 documents over an index on `x` and one on `y`."""
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3} for n in range(400)])
    db.c.create_index("x")
    db.c.create_index("y")


# The index on `y` comes first by mongod's fixed preference; the trial
# chooses the one on `x`, which reaches the 10 results in 30 keys.
_FILTER = {"x": {"$lt": 30}, "y": 1}

_READS = {
    "find": lambda d: list(d.c.find(_FILTER)),
    "aggregate": lambda d: list(d.c.aggregate([{"$match": _FILTER}])),
}


def _lines(rust_mongo: Any, mongod_rs: Any, op: Any) -> dict:
    out = {}
    for side, client, is_mongod in (("rust", rust_mongo, False), ("mongod", mongod_rs, True)):
        lines, _ = logged(client, is_mongod, _contested, op)
        out[side] = lines[0][1]
    return out


@pytest.mark.backlog("RM-598")
@pytest.mark.parametrize("command", list(_READS))
def test_a_contested_read_names_the_plan_its_trial_chose(
    rust_mongo: Any, mongod_rs: Any, command: str
) -> None:
    """mongod 8.2.11's line for a read its trial runs on the index on `x`
    names `IXSCAN { x: 1 }`, as the plan cache's entry does (measured
    2026-11-13). This server's line names the index its fixed preference
    puts first, `IXSCAN { y: 1 }`, though it ran the other."""
    got = _lines(rust_mongo, mongod_rs, _READS[command])
    answers = {side: attr.get("planSummary") for side, attr in got.items()}
    assert answers["mongod"] == "IXSCAN { x: 1 }"
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-599")
@pytest.mark.parametrize("command", list(_READS))
def test_a_contested_read_says_it_was_multi_planned(
    rust_mongo: Any, mongod_rs: Any, command: str
) -> None:
    """mongod 8.2.11's line for a read whose plan its trial chose says
    `fromMultiPlanner: true` (measured 2026-11-13). This server's line
    says nothing of how the plan was chosen."""
    got = _lines(rust_mongo, mongod_rs, _READS[command])
    answers = {
        side: {k: attr.get(k) for k in ("fromMultiPlanner", "fromPlanCache")}
        for side, attr in got.items()
    }
    assert answers["mongod"]["fromMultiPlanner"] is True
    assert answers["rust"] == answers["mongod"]
