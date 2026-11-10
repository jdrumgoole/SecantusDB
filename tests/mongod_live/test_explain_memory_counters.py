"""`explain`'s memory counters for `$group` and `$setWindowFields`, and the
`$_internalSetWindowFields` mongod parses a window into, against mongod 8.2
(RM-328).

mongod lists a `$group`'s `maxAccumulatorMemoryUsageBytes` (each
accumulator over all its groups) and `totalOutputDataSizeBytes` (what the
documents it built take), and shows `$setWindowFields` as the `$sort` and
`$_internalSetWindowFields` it runs, the `$sort` in the query's plan when it
can go there, with `maxFunctionMemoryUsageBytes` and
`maxTotalMemoryUsageBytes`. A function's own size differs between mongod's
builds (macOS arm64 and Linux, RM-397 / RM-403), so these compare an
accumulator's counter by name and the sizes that do not depend on it:
documents built field by field, a `$push`, and windows of `$first`, which
holds nothing of its own.
"""

from __future__ import annotations

from typing import Any

from tests.mongod_live.conftest import mongo_answer, same

DOCS = [{"_id": i, "k": i % 7, "s": "x" * 1000, "n": i} for i in range(300)]


def _load(client: Any) -> Any:
    client.drop_database("rm328")
    db = client.rm328
    db.c.insert_many(DOCS)
    return db


def _explained(db: Any, pipeline: list, verbosity: str = "executionStats") -> dict:
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": verbosity,
        }
    )
    stages = [
        {k: v for k, v in st.items() if k != "executionTimeMillisEstimate"}
        for st in ex["stages"][1:]
    ]
    plan = ex["stages"][0]["$cursor"]["queryPlanner"]["winningPlan"]
    return {"plan": plan, "stages": stages}


def _named(stats: dict) -> dict:
    """A stage with its per-function counters reduced to their names."""
    out = dict(stats)
    for key in ("maxAccumulatorMemoryUsageBytes", "maxFunctionMemoryUsageBytes"):
        if key in out:
            out[key] = list(out[key])
    return out


GROUPS = {
    "a key only": {"_id": "$k"},
    "two counts": {"_id": "$k", "n": {"$sum": 1}, "m": {"$sum": 1}},
    "eight counts": {"_id": "$k", **{f"f{i}": {"$sum": 1} for i in range(8)}},
    "a long name": {"_id": "$k", "a" * 40: {"$sum": 1}},
    "eight long names": {"_id": "$k", **{chr(97 + i) * 30: {"$sum": 1} for i in range(8)}},
    "one group": {"_id": None, "n": {"$sum": 1}},
}


def test_a_group_lists_its_accumulators_and_output_size(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> dict:
        db = _load(client)
        out = {}
        for name, group in GROUPS.items():
            ex = _explained(db, [{"$unwind": "$s"}, {"$group": group}])
            out[name] = [_named(st) for st in ex["stages"]]
        return out

    same(answer(rust_mongo), answer(mongod))


def test_a_push_counts_what_it_holds(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> dict:
        db = _load(client)
        return _explained(db, [{"$unwind": "$s"}, {"$group": {"_id": "$k", "a": {"$push": "$n"}}}])

    same(answer(rust_mongo), answer(mongod))


HELD = {
    "a long string": "$s",
    "a short string": {"$substrCP": ["$s", 0, 10]},
    "a longer string": {"$concat": ["$s", "$s"]},
    "a decimal": {"$toDecimal": "$n"},
    "null": None,
    "missing": "$nope",
}


def test_a_first_and_a_last_count_the_value_they_hold(rust_mongo: Any, mongod: Any) -> None:
    """The classic `$first` and `$last` count what their value keeps beyond
    its inline `Value`: a long string's buffer, a decimal's, nothing for an
    int (RM-409). Compared as differences from an int, since an
    accumulator's own size differs between mongod's builds."""

    def answer(client: Any) -> dict:
        db = _load(client)
        out = {}
        for op in ("$first", "$last"):
            group = {"_id": "$k", "int": {op: "$n"}}
            group.update({name: {op: expr} for name, expr in HELD.items()})
            stage = _explained(db, [{"$unwind": "$s"}, {"$group": group}])["stages"][-1]
            memory = stage["maxAccumulatorMemoryUsageBytes"]
            out[op] = {name: memory[name] - memory["int"] for name in HELD}
        return out

    same(answer(rust_mongo), answer(mongod))


WINDOWS = {
    "leading, sorted": [
        {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"z": {"$first": "$n"}}}}
    ],
    "partitioned, bounded": [
        {
            "$setWindowFields": {
                "partitionBy": "$k",
                "sortBy": {"n": 1},
                "output": {"z": {"$first": "$n", "window": {"documents": [-1, 1]}}},
            }
        }
    ],
    "partitioned, whole partition": [
        {"$setWindowFields": {"partitionBy": "$k", "output": {"z": {"$first": "$n"}}}}
    ],
    "a sliding window": [
        {
            "$setWindowFields": {
                "sortBy": {"n": 1},
                "output": {"z": {"$first": "$n", "window": {"documents": [-2, 3]}}},
            }
        }
    ],
    "functions named in order": [
        {
            "$setWindowFields": {
                "sortBy": {"n": -1},
                "output": {
                    "z": {"$first": "$n"},
                    "a": {"$first": "$n"},
                    "m": {"$last": "$n"},
                },
            }
        }
    ],
    "unsorted": [{"$setWindowFields": {"output": {"z": {"$first": "$n"}}}}],
}


def test_a_window_is_its_sort_and_internal_stage(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> dict:
        db = _load(client)
        return {name: _explained(db, p) for name, p in WINDOWS.items()}

    same(answer(rust_mongo), answer(mongod))


def test_a_window_over_its_budget_peaks_before_it_spills(rust_mongo: Any, mongod: Any) -> None:
    knob = "internalDocumentSourceSetWindowFieldsMaxMemoryBytes"

    def answer(client: Any) -> dict:
        db = _load(client)
        client.admin.command({"setParameter": 1, knob: 100_000})
        try:
            stage = _explained(db, WINDOWS["leading, sorted"])["stages"][0]
        finally:
            client.admin.command({"setParameter": 1, knob: 104_857_600})
        # The spill file's size is RM-325's.
        stage.pop("spilledDataStorageSize")
        return stage

    same(answer(rust_mongo), answer(mongod))


def test_a_window_behind_an_unwind_sorts_in_the_query(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> dict:
        db = _load(client)
        ex = _explained(
            db,
            [
                {"$unwind": "$s"},
                {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"t": {"$sum": "$n"}}}},
            ],
        )
        # What a document the `$unwind` changed takes is not mongod's yet.
        ex["stages"][-1].pop("maxTotalMemoryUsageBytes")
        return {"plan": ex["plan"], "stages": [_named(st) for st in ex["stages"]]}

    same(answer(rust_mongo), answer(mongod))


OUTPUTS = {
    "a constant": {"$sum": 1},
    "an object": {"$push": {"a": "$n", "b": 1}},
    "an expression": {"$sum": {"$add": ["$n", 1]}},
    "$shift": {"$shift": {"output": "$n", "by": 1}},
    "$shift with a default": {"$shift": {"output": "$n", "by": -1, "default": 0}},
    "$expMovingAvg N": {"$expMovingAvg": {"input": "$n", "N": 2}},
    "$expMovingAvg alpha": {"$expMovingAvg": {"input": "$n", "alpha": 0.5}},
    "$locf": {"$locf": "$n"},
    "$linearFill": {"$linearFill": "$n"},
    "$derivative": {"$derivative": {"input": "$n"}, "window": {"documents": [-1, 0]}},
    "$integral": {"$integral": {"input": "$n"}},
    "$topN": {"$topN": {"n": 2, "sortBy": {"n": -1}, "output": "$n"}},
    "$top": {"$top": {"sortBy": {"n": -1}, "output": "$n"}},
    "$firstN": {"$firstN": {"n": 2, "input": "$n"}},
    "a document window": {"$first": "$n", "window": {"documents": [-1, 1]}},
    "a range window": {"$sum": "$n", "window": {"range": [-1, "current"]}},
    "$covariancePop": {"$covariancePop": ["$n", "$n"]},
    "$count": {"$count": {}},
    "$rank": {"$rank": {}},
    "$documentNumber": {"$documentNumber": {}},
}


def test_window_outputs_are_listed_as_mongod_prints_them(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> dict:
        db = _load(client)
        out = {}
        for name, spec in OUTPUTS.items():
            pipeline = [{"$setWindowFields": {"sortBy": {"n": 1}, "output": {"x": spec}}}]
            out[name] = _explained(db, pipeline, "queryPlanner")["stages"]
        return out

    same(answer(rust_mongo), answer(mongod))


PARTITIONS = {
    "a computed key": [
        {
            "$setWindowFields": {
                "partitionBy": {"$mod": ["$n", 3]},
                "sortBy": {"n": 1},
                "output": {"x": {"$sum": 1}},
            }
        }
    ],
    "a literal": [
        {"$setWindowFields": {"partitionBy": {"$literal": "a"}, "output": {"x": {"$sum": 1}}}}
    ],
    "a constant expression": [
        {"$setWindowFields": {"partitionBy": {"$add": [1, 2]}, "output": {"x": {"$sum": 1}}}}
    ],
    "a dotted path": [{"$setWindowFields": {"partitionBy": "$a.b", "output": {"x": {"$sum": 1}}}}],
    "a $$ROOT path": [
        {"$setWindowFields": {"partitionBy": "$$ROOT.k", "output": {"x": {"$sum": 1}}}}
    ],
    "only a partition": [
        {"$setWindowFields": {"partitionBy": "$k", "sortBy": {"n": 1}, "output": {}}}
    ],
    "after an $addFields": [
        {"$addFields": {"z": 1}},
        {"$setWindowFields": {"sortBy": {"z": 1}, "output": {"t": {"$sum": "$n"}}}},
    ],
    "in a $facet": [
        {
            "$facet": {
                "f": [
                    {
                        "$setWindowFields": {
                            "partitionBy": {"$mod": ["$n", 2]},
                            "sortBy": {"n": 1},
                            "output": {"x": {"$sum": 1}},
                        }
                    }
                ]
            }
        }
    ],
}


def test_a_partition_is_sorted_on_as_mongod_parses_it(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> dict:
        db = _load(client)
        return {name: _explained(db, p, "queryPlanner") for name, p in PARTITIONS.items()}

    same(answer(rust_mongo), answer(mongod))


def test_internal_set_window_fields_runs_over_its_input_as_it_comes(
    rust_mongo: Any, mongod: Any
) -> None:
    """A pipeline may name the stage itself: it does not sort, and a
    partition is a run of documents with one key."""

    def answer(client: Any) -> dict:
        client.drop_database("rm328i")
        db = client.rm328i
        db.c.insert_many([{"_id": i, "k": [1, 2, 1, 2, 2, 1][i], "n": i} for i in range(6)])
        runs = {
            "runs of keys": [
                {
                    "$_internalSetWindowFields": {
                        "partitionBy": "$k",
                        "sortBy": {"n": 1},
                        "output": {"t": {"$sum": "$n"}, "d": {"$push": "$n"}},
                    }
                }
            ],
            "input order": [
                {"$sort": {"n": -1}},
                {
                    "$_internalSetWindowFields": {
                        "sortBy": {"n": 1},
                        "output": {"t": {"$sum": "$n", "window": {"documents": [-1, 0]}}},
                    }
                },
            ],
            "not an object": [{"$_internalSetWindowFields": 5}],
            "an unknown field": [{"$_internalSetWindowFields": {"output": {}, "x": 1}}],
            "no output": [{"$_internalSetWindowFields": {"sortBy": {"n": 1}}}],
            "a sortBy of the wrong type": [
                {"$_internalSetWindowFields": {"sortBy": 1, "output": {}}}
            ],
            "an unknown function": [
                {"$_internalSetWindowFields": {"output": {"t": {"$bogus": 1}}}}
            ],
        }
        return {name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in runs.items()}

    same(answer(rust_mongo), answer(mongod))
