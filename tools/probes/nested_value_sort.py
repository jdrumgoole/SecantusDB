"""Sort ORDER over embedded documents and arrays, against mongod.

The index / sort-key encoding writes a document or an array as raw BSON, and
raw BSON leads with a little-endian LENGTH -- so byte order ranks
`{a: 2, b: [3]}` above `{a: 5}` because it is longer, not because it compares
higher. The Rust server's `find().sort()` sorted by those bytes and was wrong
on 5 of 7 shapes below (measured 8.2.11, 2026-09-30), while the aggregation
`$sort`, which compares values, was right on all of them. An index on the
field inherited the same order through its walk.

Each shape runs as `find` + sort, as `aggregate` + `$sort`, and again with an
index on the field, and the ORDER is compared -- `index_result_sets.py`
compares sets and cannot see this.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27055/?directConnection=true" \\
        python tools/probes/nested_value_sort.py
"""

from __future__ import annotations

import os
import sys

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041/?directConnection=true")
SERVER = os.environ.get("PROBE_SERVER")

#: Value lists; each becomes one collection of `{_id: i, x: value}`.
SHAPES = [
    [[[5]], [1, [2, [3]]]],
    [[[9]], [[2, [3]]]],
    [[[1], [9]], [1, [2, [3]]]],
    [{"a": 5}, {"a": 2, "b": [3]}],
    [[{"a": 5}], [{"a": 2, "b": 1}]],
    [{"a": 1, "b": 1}, {"a": "x"}],
    [[1, 2], [1, 2, 3]],
    [[3], [1, 2, 3]],
    [
        {"a": 5},
        {"a": 2, "b": [3]},
        {"a": 3},
        {"a": 2, "b": 1, "c": "long string here"},
        {"a": 9, "z": {"q": [1, 2, 3, 4, 5, 6]}},
    ],
    [{"a": {"b": 1}}, {"a": {"b": 1, "c": 2}}, {"a": {"a": 9}}, {"a": 1}],
    # TYPE rank before field NAME, element by element (mongod's woCompare):
    # an object under `crs` sorts before an array under `coordinates`, although
    # `coordinates` is the smaller name. Found through GeoJSON with and without
    # a `crs` member (2026-10-06).
    [{"t": "P", "coordinates": [3]}, {"t": "P", "crs": {"n": "b"}}, {"t": "P", "crs": {"n": "a"}}],
    [{"z": 1}, {"a": "s"}, {"m": None}, {"b": [1]}],
]


#: Shapes where mongod disagrees with ITSELF across plans, so there is nothing to
#: match: over `[[3], [1, 2, 3]]` with a multikey index on `x`,
#: `$sort: {x: -1, _id: 1}` returns `[1, 0]` -- both documents' descending key is
#: 3, so the `_id` tiebreak says `[0, 1]`, which is what mongod answers without
#: the index (measured 8.2.11, 2026-09-30). Reported, not counted.
KNOWN_MONGOD_PLAN_ARTIFACTS = [([[3], [1, 2, 3]], True)]


def orders(coll, values, indexed):
    coll.drop()
    coll.insert_many([{"_id": i, "x": v} for i, v in enumerate(values)])
    if indexed:
        coll.create_index("x")
    out = []
    for direction in (1, -1):
        out.append([d["_id"] for d in coll.find().sort("x", direction)])
        out.append([d["_id"] for d in coll.aggregate([{"$sort": {"x": direction, "_id": 1}}])])
    return out


def main() -> int:
    if not SERVER:
        print("PROBE_SERVER is required: this probe compares a running server with mongod")
        return 2
    mongod = pymongo.MongoClient(MONGOD).probe_nested_sort.c
    ours = pymongo.MongoClient(SERVER).probe_nested_sort.c
    total = bad = 0
    for values in SHAPES:
        for indexed in (False, True):
            total += 1
            want, got = orders(mongod, values, indexed), orders(ours, values, indexed)
            if want != got and (values, indexed) in KNOWN_MONGOD_PLAN_ARTIFACTS:
                print(f"KNOWN (mongod plan artifact) indexed={indexed} {values}")
            elif want != got:
                bad += 1
                print(f"DIFF indexed={indexed} {values}\n  mongod: {want}\n  ours:   {got}")
    for c in (mongod, ours):
        c.database.client.drop_database("probe_nested_sort")
    print(f"=== nested-value sort order: {bad} of {total} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
