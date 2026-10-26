"""TEMPORARY: read mongod's accumulator and window-function sizes on the CI
(Linux) build. Fails on purpose to print them; removed before merging."""

from __future__ import annotations

import json
from typing import Any

ACCS = {
    "sum": {"$sum": "$v"},
    "count": {"$count": {}},
    "avg": {"$avg": "$v"},
    "min": {"$min": "$v"},
    "max": {"$max": "$s"},
    "first": {"$first": "$s"},
    "last": {"$last": "$v"},
    "push": {"$push": "$v"},
    "pushs": {"$push": "$s"},
    "addToSet": {"$addToSet": "$v"},
    "merge": {"$mergeObjects": "$o"},
    "sd": {"$stdDevPop": "$v"},
    "firstN": {"$firstN": {"n": 2, "input": "$v"}},
    "maxN": {"$maxN": {"n": 2, "input": "$v"}},
    "top": {"$top": {"sortBy": {"v": 1}, "output": "$v"}},
    "topN": {"$topN": {"n": 2, "sortBy": {"v": 1}, "output": "$v"}},
    "median": {"$median": {"input": "$v", "method": "approximate"}},
    "cat": {"$concatArrays": {"$ifNull": [["$_id"], []]}},
    "union": {"$setUnion": {"$ifNull": [["$_id"], []]}},
}
FUNCS = {
    "sum": {"$sum": "$n"},
    "avg": {"$avg": "$n"},
    "push": {"$push": "$n"},
    "sd": {"$stdDevPop": "$n"},
    "max": {"$max": "$s"},
}
WINDOWS = {
    "none": None,
    "pm5": {"documents": [-5, 5]},
    "uc": {"documents": ["unbounded", "current"]},
}


def test_print_linux_sizes(mongod: Any) -> None:
    db = mongod.linuxprobe
    db.c.drop()
    db.c.insert_many(
        [
            {
                "_id": i,
                "v": i,
                "n": i * 2,
                "s": "abcdefghijklmnopqrstuvwxyz"[: i + 12],
                "o": {"a": i},
            }
            for i in range(5)
        ]
    )
    spec: dict = {"_id": None, **ACCS}
    ex = db.command(
        {
            "explain": {
                "aggregate": "c",
                "pipeline": [{"$unwind": "$s"}, {"$group": spec}],
                "cursor": {},
            },
            "verbosity": "executionStats",
        }
    )
    out: dict = {
        "group": [s.get("maxAccumulatorMemoryUsageBytes") for s in ex["stages"] if "$group" in s]
    }
    for w, window in WINDOWS.items():
        for f, fn in FUNCS.items():
            sp = dict(fn)
            if window:
                sp["window"] = window
            p = [
                {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"t": sp}}},
                {"$project": {"_id": 1, "t": 1}},
            ]
            ex = db.command(
                {
                    "explain": {"aggregate": "c", "pipeline": p, "cursor": {}},
                    "verbosity": "executionStats",
                }
            )
            st = [s for s in ex["stages"] if "$_internalSetWindowFields" in s][0]
            out[f"{w}/{f}"] = (
                st["maxFunctionMemoryUsageBytes"]["t"],
                st["maxTotalMemoryUsageBytes"],
            )
    raise AssertionError("LINUXSIZES " + json.dumps(out))
