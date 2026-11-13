"""What a `$sort` counts for the documents an inclusion `$project` builds
(RM-519).

mongod 8.2's inclusion `$project` (`InclusionNode`) builds each document
it returns in a `MutableDocument` with room for every field the spec names
-- `_id` and a missing field too -- copies each field it keeps in the
input's order, projects a sub-document into a document of its own and an
array element by element, then sets each computed field. A document no
stage wrote to, with nothing to compute, goes straight from its BSON into
new BSON instead. A field it copies shares the input's value, so a computed
field reading inside a copied sub-document grows it. The `$sort` after it
counts that storage. Compared is the
`$sort`'s `totalDataSizeSortedBytesEstimate` in `explain`, and the window's
sort and cache behind a `$project`. Measured on 8.2.11, 2026-11-13.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import same

UNWIND = {"$unwind": "$t"}
ARRAY = {"r": [{"x": 1, "y": 2}, 3, {"y": 1}]}

#: name -> (stages before the `$sort`, fields added to each document, `t`).
CASES: dict[str, tuple[list[dict], dict, Any]] = {
    "copied": ([UNWIND, {"$project": {"t": 1, "n": 1}}], {}, [1, "z" * 30, {"q": 1}]),
    "missing_field": ([UNWIND, {"$project": {"t": 1, "n": 1, "zz": 1, "yy": 1}}], {}, [1]),
    "no_id": ([UNWIND, {"$project": {"t": 1, "n": 1, "_id": 0}}], {}, [1]),
    "id_only": ([UNWIND, {"$project": {"_id": 1}}], {}, [1]),
    "computed": ([UNWIND, {"$project": {"t": 1, "x": {"$concat": ["$b", "!"]}}}], {}, [1]),
    "computed_id": ([UNWIND, {"$project": {"_id": {"$add": ["$_id", 1]}, "t": 1}}], {}, [1]),
    "computed_object": ([UNWIND, {"$project": {"n": 1, "o": {"x": "$n"}}}], {}, [1]),
    "computed_array": ([UNWIND, {"$project": {"n": 1, "o": ["$n", "$a"]}}], {}, [1]),
    "computed_reference": ([UNWIND, {"$project": {"n": 1, "o": "$e"}}], {}, [1]),
    "computed_missing": ([UNWIND, {"$project": {"n": 1, "o": "$nope"}}], {}, [1]),
    "literal": ([UNWIND, {"$project": {"n": 1, "o": "lit", "p": None}}], {}, [1]),
    "sub_document": ([UNWIND, {"$project": {"t": 1, "e.x": 1, "e.y": 1}}], {}, [1]),
    "nested": ([UNWIND, {"$project": {"t": 1, "e.z.k": 1}}], {}, [1]),
    "sub_computed": ([UNWIND, {"$project": {"t": 1, "e.q": {"$add": [1, 1]}}}], {}, [1]),
    "child_of_scalar": ([UNWIND, {"$project": {"a.x": 1, "n": 1}}], {}, [1]),
    "computed_child_of_scalar": (
        [UNWIND, {"$project": {"a.q": {"$add": [1, 1]}, "n": 1}}],
        {},
        [1],
    ),
    "array": ([UNWIND, {"$project": {"r.x": 1, "n": 1}}], ARRAY, [1]),
    "array_computed": (
        [UNWIND, {"$project": {"r.x": 1, "r.q": {"$add": [1, 1]}, "n": 1}}],
        ARRAY,
        [1],
    ),
    "nested_array": ([UNWIND, {"$project": {"r.x": 1, "n": 1}}], {"r": [[{"x": 1}], 2]}, [1]),
    "unchanged_input": ([UNWIND, {"$project": {"e.x": 1, "n": 1}}], {}, 5),
    "unchanged_input_computed": (
        [UNWIND, {"$project": {"t": 1, "x": {"$add": [1, 1]}}}],
        {},
        5,
    ),
    "built_by_add_fields": (
        [
            UNWIND,
            {"$addFields": {"o": {"x": "$n", "y": "$b"}}},
            {"$project": {"o": 1, "p": "$o", "n": 1}},
        ],
        {},
        [1],
    ),
    # A copied sub-document shares the storage a computed field's read of
    # it caches into (RM-515), cached by `$addFields` or by the copy itself.
    "shares_a_cached_sub_document": (
        [UNWIND, {"$addFields": {"q": "$e.x"}}, {"$project": {"e": 1, "n": 1, "w": "$e.y"}}],
        {},
        [1],
    ),
    "shares_a_copied_sub_document": (
        [UNWIND, {"$project": {"n": 1, "e": 1, "w": "$e.z.k"}}],
        {},
        [1],
    ),
    "shares_a_referenced_sub_document": (
        [UNWIND, {"$project": {"n": 1, "w": "$e", "v": "$e.y"}}],
        {},
        [1],
    ),
    "then_set": ([UNWIND, {"$project": {"t": 1, "n": 1}}, {"$set": {"w": "$n"}}], {}, [1]),
    "then_project": (
        [UNWIND, {"$project": {"t": 1, "n": 1, "b": 1}}, {"$project": {"n": 1, "b": 1}}],
        {},
        [1],
    ),
    "after_unwound_group": (
        [UNWIND, {"$group": {"_id": "$n", "c": {"$sum": 1}}}, {"$project": {"c": 1}}],
        {},
        [1],
    ),
}


def _load(client: Any, extra: dict, t: Any) -> Any:
    client.drop_database("projectstorage")
    db = client.projectstorage
    base = {"a": 5, "b": "y" * 100, "e": {"x": 1, "y": "w" * 50, "z": {"k": 1}}, "t": t}
    db.c.insert_many([{"_id": i, **base, **extra, "n": i} for i in range(20)])
    return db


def _sizes(client: Any, case: str) -> list:
    stages, extra, t = CASES[case]
    db = _load(client, extra, t)
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": [*stages, {"$sort": {"n": 1}}], "cursor": {}},
            "verbosity": "executionStats",
        }
    )
    return [(next(iter(s)), s.get("totalDataSizeSortedBytesEstimate")) for s in ex["stages"][1:]]


@pytest.mark.parametrize("case", sorted(CASES))
def test_a_sort_counts_what_a_project_built(rust_mongo: Any, mongod: Any, case: str) -> None:
    same(_sizes(rust_mongo, case), _sizes(mongod, case))


def _window(client: Any) -> list:
    db = _load(client, {}, [1, "z" * 30])
    pipeline = [
        UNWIND,
        {"$project": {"t": 1, "n": 1, "x": {"$add": [1, 1]}}},
        {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"z": {"$first": "$n"}}}},
    ]
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": "executionStats",
        }
    )
    return [
        (
            next(iter(s)),
            s.get("totalDataSizeSortedBytesEstimate"),
            s.get("maxTotalMemoryUsageBytes"),
        )
        for s in ex["stages"][1:]
    ]


def test_a_window_counts_what_a_project_built(rust_mongo: Any, mongod: Any) -> None:
    same(_window(rust_mongo), _window(mongod))
