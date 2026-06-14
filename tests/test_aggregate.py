from __future__ import annotations

import pytest
from bson import Int64

from secantus.aggregate import AggregateError, apply_pipeline


def test_group_sum_preserves_int64_type() -> None:
    """$sum over Int64 values stays Int64 (mongod widens int32 < int64),
    not a bare int that narrows to int32 on the wire."""
    out = apply_pipeline(
        [{"q": Int64(10)}, {"q": Int64(10)}],
        [{"$group": {"_id": None, "t": {"$sum": "$q"}}}],
    )
    assert out[0]["t"] == 20 and isinstance(out[0]["t"], Int64)


def test_match_filters_docs() -> None:
    docs = [{"x": 1}, {"x": 2}, {"x": 3}]
    out = apply_pipeline(docs, [{"$match": {"x": {"$gte": 2}}}])
    assert [d["x"] for d in out] == [2, 3]


def test_sort_pipeline_stage() -> None:
    docs = [{"x": 3}, {"x": 1}, {"x": 2}]
    out = apply_pipeline(docs, [{"$sort": {"x": 1}}])
    assert [d["x"] for d in out] == [1, 2, 3]


def test_project_inclusion_keeps_id_by_default() -> None:
    docs = [{"_id": 1, "a": 1, "b": 2}]
    out = apply_pipeline(docs, [{"$project": {"a": 1}}])
    assert out == [{"_id": 1, "a": 1}]


def test_project_with_computed_field() -> None:
    docs = [{"x": 3, "y": 4}]
    out = apply_pipeline(docs, [{"$project": {"_id": 0, "sum": {"$add": ["$x", "$y"]}}}])
    assert out == [{"sum": 7}]


def test_project_rename_via_field_path() -> None:
    docs = [{"_id": 1, "a": "hello"}]
    out = apply_pipeline(docs, [{"$project": {"_id": 0, "renamed": "$a"}}])
    assert out == [{"renamed": "hello"}]


def test_add_fields() -> None:
    docs = [{"x": 1}]
    out = apply_pipeline(docs, [{"$addFields": {"y": 2, "doubled": {"$multiply": ["$x", 2]}}}])
    assert out == [{"x": 1, "y": 2, "doubled": 2}]


def test_set_alias_for_add_fields() -> None:
    docs = [{"x": 1}]
    out = apply_pipeline(docs, [{"$set": {"y": 2}}])
    assert out == [{"x": 1, "y": 2}]


def test_unset_string_form() -> None:
    docs = [{"a": 1, "b": 2}]
    out = apply_pipeline(docs, [{"$unset": "b"}])
    assert out == [{"a": 1}]


def test_unset_list_form() -> None:
    docs = [{"a": 1, "b": 2, "c": 3}]
    out = apply_pipeline(docs, [{"$unset": ["a", "c"]}])
    assert out == [{"b": 2}]


def test_unwind_simple_path() -> None:
    docs = [{"_id": 1, "tags": ["a", "b", "c"]}]
    out = apply_pipeline(docs, [{"$unwind": "$tags"}])
    assert [d["tags"] for d in out] == ["a", "b", "c"]


def test_unwind_with_index() -> None:
    docs = [{"_id": 1, "tags": ["x", "y"]}]
    out = apply_pipeline(docs, [{"$unwind": {"path": "$tags", "includeArrayIndex": "i"}}])
    assert [(d["tags"], d["i"]) for d in out] == [("x", 0), ("y", 1)]


def test_unwind_empty_array_drops_doc_by_default() -> None:
    docs = [{"_id": 1, "tags": []}, {"_id": 2, "tags": ["x"]}]
    out = apply_pipeline(docs, [{"$unwind": "$tags"}])
    assert [d["_id"] for d in out] == [2]


def test_unwind_preserve_null() -> None:
    docs = [{"_id": 1, "tags": []}, {"_id": 2}, {"_id": 3, "tags": ["x"]}]
    out = apply_pipeline(docs, [{"$unwind": {"path": "$tags", "preserveNullAndEmptyArrays": True}}])
    assert sorted(d["_id"] for d in out) == [1, 2, 3]


def test_replace_root() -> None:
    docs = [{"_id": 1, "inner": {"a": 1, "b": 2}}]
    out = apply_pipeline(docs, [{"$replaceRoot": {"newRoot": "$inner"}}])
    assert out == [{"a": 1, "b": 2}]


def test_replace_with_alias() -> None:
    docs = [{"inner": {"a": 1}}]
    out = apply_pipeline(docs, [{"$replaceWith": "$inner"}])
    assert out == [{"a": 1}]


def test_group_sum_count() -> None:
    docs = [
        {"team": "a", "score": 10},
        {"team": "a", "score": 5},
        {"team": "b", "score": 7},
    ]
    out = apply_pipeline(
        docs,
        [{"$group": {"_id": "$team", "total": {"$sum": "$score"}, "n": {"$sum": 1}}}],
    )
    by_id = {d["_id"]: d for d in out}
    assert by_id["a"]["total"] == 15
    assert by_id["a"]["n"] == 2
    assert by_id["b"]["total"] == 7
    assert by_id["b"]["n"] == 1


def test_group_avg() -> None:
    docs = [{"x": 2}, {"x": 4}, {"x": 6}]
    out = apply_pipeline(docs, [{"$group": {"_id": None, "avg": {"$avg": "$x"}}}])
    assert out == [{"_id": None, "avg": 4.0}]


def test_group_min_max_first_last_push_addtoset() -> None:
    docs = [{"x": 3}, {"x": 1}, {"x": 2}, {"x": 1}]
    out = apply_pipeline(
        docs,
        [
            {
                "$group": {
                    "_id": None,
                    "min": {"$min": "$x"},
                    "max": {"$max": "$x"},
                    "first": {"$first": "$x"},
                    "last": {"$last": "$x"},
                    "all": {"$push": "$x"},
                    "set": {"$addToSet": "$x"},
                }
            }
        ],
    )
    [bucket] = out
    assert bucket["min"] == 1
    assert bucket["max"] == 3
    assert bucket["first"] == 3
    assert bucket["last"] == 1
    assert bucket["all"] == [3, 1, 2, 1]
    assert sorted(bucket["set"]) == [1, 2, 3]


def test_pipeline_chain_match_sort_project_limit() -> None:
    docs = [{"_id": i, "x": i, "name": f"n{i}"} for i in range(10)]
    out = apply_pipeline(
        docs,
        [
            {"$match": {"x": {"$gte": 5}}},
            {"$sort": {"x": -1}},
            {"$project": {"_id": 0, "x": 1}},
            {"$limit": 3},
        ],
    )
    assert out == [{"x": 9}, {"x": 8}, {"x": 7}]


def test_unsupported_stage_raises() -> None:
    with pytest.raises(AggregateError):
        apply_pipeline([{"x": 1}], [{"$bogusStage": {}}])


def test_project_mixed_inclusion_exclusion_rejected() -> None:
    with pytest.raises(AggregateError):
        apply_pipeline([{"a": 1, "b": 2}], [{"$project": {"a": 1, "b": 0}}])


def test_sample_returns_subset() -> None:
    docs = [{"i": i} for i in range(10)]
    out = apply_pipeline(docs, [{"$sample": {"size": 3}}])
    assert len(out) == 3
    assert all(d in docs for d in out)


def test_sample_size_larger_than_input_returns_all() -> None:
    docs = [{"i": i} for i in range(3)]
    out = apply_pipeline(docs, [{"$sample": {"size": 10}}])
    assert len(out) == 3


def test_sample_with_secantus_sample_seed_is_deterministic(monkeypatch) -> None:
    """Setting ``SECANTUS_SAMPLE_SEED`` makes ``$sample`` reproducible.

    The seed is captured at module import time, so changing it
    mid-process via env-var alone won't help — we have to rebuild the
    RNG via the private helper. This test pins the deterministic-
    behaviour contract that the env-var injects a dedicated
    ``random.Random(seed)`` (rather than seeding the module-level
    ``random`` and leaking state into other code in the same process).
    """
    import secantus.aggregate as agg

    monkeypatch.setenv("SECANTUS_SAMPLE_SEED", "42")
    monkeypatch.setattr(agg, "_SAMPLE_RNG", agg._build_sample_rng())

    docs = [{"i": i} for i in range(20)]
    a = apply_pipeline(docs, [{"$sample": {"size": 5}}])
    # Reset the RNG to the same seed and re-run — must be identical.
    monkeypatch.setattr(agg, "_SAMPLE_RNG", agg._build_sample_rng())
    b = apply_pipeline(docs, [{"$sample": {"size": 5}}])
    assert a == b
    assert len(a) == 5


def test_sort_by_count() -> None:
    docs = [{"t": "a"}, {"t": "b"}, {"t": "a"}, {"t": "c"}, {"t": "a"}]
    out = apply_pipeline(docs, [{"$sortByCount": "$t"}])
    assert out[0] == {"_id": "a", "count": 3}
    assert {d["_id"] for d in out} == {"a", "b", "c"}


def test_facet_runs_parallel_pipelines() -> None:
    docs = [{"x": 1}, {"x": 2}, {"x": 3}, {"x": 4}]
    out = apply_pipeline(
        docs,
        [
            {
                "$facet": {
                    "all": [{"$count": "n"}],
                    "big": [{"$match": {"x": {"$gte": 3}}}, {"$count": "n"}],
                }
            }
        ],
    )
    assert out == [{"all": [{"n": 4}], "big": [{"n": 2}]}]


def test_bucket_basic_ranges() -> None:
    docs = [{"v": 1}, {"v": 5}, {"v": 12}, {"v": 25}, {"v": 99}]
    out = apply_pipeline(
        docs,
        [
            {
                "$bucket": {
                    "groupBy": "$v",
                    "boundaries": [0, 10, 20, 30],
                    "default": "other",
                    "output": {"count": {"$sum": 1}},
                }
            }
        ],
    )
    by_id = {d["_id"]: d["count"] for d in out}
    assert by_id[0] == 2
    assert by_id[10] == 1
    assert by_id[20] == 1
    assert by_id["other"] == 1


def test_bucket_auto_even_split() -> None:
    docs = [{"v": i} for i in range(10)]
    out = apply_pipeline(docs, [{"$bucketAuto": {"groupBy": "$v", "buckets": 5}}])
    assert len(out) == 5
    assert sum(b["count"] for b in out) == 10
    assert out[0]["_id"]["min"] == 0


def test_bucket_auto_with_output() -> None:
    docs = [{"v": i, "n": i * 2} for i in range(8)]
    out = apply_pipeline(
        docs,
        [
            {
                "$bucketAuto": {
                    "groupBy": "$v",
                    "buckets": 4,
                    "output": {"total_n": {"$sum": "$n"}},
                }
            }
        ],
    )
    assert len(out) == 4
    assert sum(b["total_n"] for b in out) == sum(d["n"] for d in docs)


def test_lookup_requires_storage_context() -> None:
    with pytest.raises(AggregateError):
        apply_pipeline(
            [{"x": 1}],
            [
                {
                    "$lookup": {
                        "from": "other",
                        "localField": "x",
                        "foreignField": "x",
                        "as": "joined",
                    }
                }
            ],
        )


# ---------------------------------------------------------------------
# $lookup hash-join correctness, especially for array-valued fields.


def _setup_lookup_storage(tmp_path, outer_docs, foreign_docs, foreign_coll="f"):
    from secantus.aggregate import PipelineContext
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    storage.insert("db", foreign_coll, foreign_docs)
    ctx = PipelineContext(storage=storage, db_name="db")
    return storage, ctx


def test_lookup_simple_form_hash_join_correctness(tmp_path) -> None:
    foreign = [
        {"_id": "abc", "stock": 100},
        {"_id": "xyz", "stock": 50},
    ]
    storage, ctx = _setup_lookup_storage(tmp_path, [], foreign, foreign_coll="inv")
    docs = [
        {"_id": 1, "item": "abc"},
        {"_id": 2, "item": "xyz"},
        {"_id": 3, "item": "missing"},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$lookup": {
                    "from": "inv",
                    "localField": "item",
                    "foreignField": "_id",
                    "as": "j",
                }
            }
        ],
        ctx,
    )
    assert [d["j"] for d in out] == [
        [{"_id": "abc", "stock": 100}],
        [{"_id": "xyz", "stock": 50}],
        [],
    ]


def test_lookup_foreign_field_is_array_each_element_matches(tmp_path) -> None:
    """Foreign doc with an array foreign_field matches against any element."""
    foreign = [
        {"_id": "x", "tags": ["python", "go"]},
        {"_id": "y", "tags": ["rust"]},
    ]
    storage, ctx = _setup_lookup_storage(tmp_path, [], foreign, foreign_coll="f")
    docs = [{"_id": 1, "want": "python"}, {"_id": 2, "want": "rust"}]
    out = apply_pipeline(
        docs,
        [
            {
                "$lookup": {
                    "from": "f",
                    "localField": "want",
                    "foreignField": "tags",
                    "as": "j",
                }
            }
        ],
        ctx,
    )
    assert [fd["_id"] for fd in out[0]["j"]] == ["x"]
    assert [fd["_id"] for fd in out[1]["j"]] == ["y"]


def test_lookup_local_field_is_array_each_element_lookups(tmp_path) -> None:
    foreign = [
        {"_id": "py", "lang": "python"},
        {"_id": "go", "lang": "go"},
        {"_id": "rs", "lang": "rust"},
    ]
    storage, ctx = _setup_lookup_storage(tmp_path, [], foreign, foreign_coll="f")
    docs = [{"_id": 1, "wants": ["python", "rust"]}]
    out = apply_pipeline(
        docs,
        [
            {
                "$lookup": {
                    "from": "f",
                    "localField": "wants",
                    "foreignField": "lang",
                    "as": "j",
                }
            }
        ],
        ctx,
    )
    found = sorted(fd["_id"] for fd in out[0]["j"])
    assert found == ["py", "rs"]


def test_lookup_both_sides_arrays_intersection_match(tmp_path) -> None:
    foreign = [
        {"_id": "x", "tags": ["a", "b"]},
        {"_id": "y", "tags": ["c", "d"]},
        {"_id": "z", "tags": ["b", "c"]},
    ]
    storage, ctx = _setup_lookup_storage(tmp_path, [], foreign, foreign_coll="f")
    docs = [{"_id": 1, "want": ["a", "c"]}]
    out = apply_pipeline(
        docs,
        [
            {
                "$lookup": {
                    "from": "f",
                    "localField": "want",
                    "foreignField": "tags",
                    "as": "j",
                }
            }
        ],
        ctx,
    )
    found = sorted(fd["_id"] for fd in out[0]["j"])
    # x matches via "a", y matches via "c", z matches via "c".
    assert found == ["x", "y", "z"]


def test_lookup_hash_join_does_not_call_lookup_match_for_hashable_values(
    monkeypatch, tmp_path
) -> None:
    """O(N+M) hash-join: per-pair _lookup_match should not fire for hashable scalars."""
    import secantus.aggregate as agg

    foreign = [{"_id": i, "k": i} for i in range(50)]
    storage, ctx = _setup_lookup_storage(tmp_path, [], foreign, foreign_coll="f")
    docs = [{"_id": j, "k": j % 50} for j in range(20)]

    call_count = [0]
    real = agg._lookup_match

    def counting(local, foreign):
        call_count[0] += 1
        return real(local, foreign)

    monkeypatch.setattr(agg, "_lookup_match", counting)
    apply_pipeline(
        docs,
        [
            {
                "$lookup": {
                    "from": "f",
                    "localField": "k",
                    "foreignField": "k",
                    "as": "j",
                }
            }
        ],
        ctx,
    )
    # With nested-loop O(N×M) we'd see 20 × 50 = 1000 calls. Hash-join
    # uses dict lookups for hashable scalars and never reaches _lookup_match.
    assert call_count[0] == 0


def test_lookup_pipeline_form_simple_prefilter_hash_join(tmp_path) -> None:
    """Pipeline form with localField+foreignField also hash-joins the prefilter."""
    foreign = [{"_id": i, "user_id": i % 5, "v": i} for i in range(20)]
    storage, ctx = _setup_lookup_storage(tmp_path, [], foreign, foreign_coll="orders")
    docs = [{"_id": "u3", "uid": 3}]
    out = apply_pipeline(
        docs,
        [
            {
                "$lookup": {
                    "from": "orders",
                    "localField": "uid",
                    "foreignField": "user_id",
                    "let": {"u": "$uid"},
                    "pipeline": [{"$match": {"$expr": {"$gt": ["$v", 5]}}}],
                    "as": "j",
                }
            }
        ],
        ctx,
    )
    matched_v = sorted(fd["v"] for fd in out[0]["j"])
    # user_id=3 → v ∈ {3, 8, 13, 18}. After $gt: 5 → {8, 13, 18}.
    assert matched_v == [8, 13, 18]


# ----------------------------------------------------------------------
# $densify (numeric): fills gaps between consecutive values in the input
# with filler docs that have only the densify field set.


def test_densify_full_bounds_fills_inner_gaps() -> None:
    docs = [{"_id": 1, "n": 1}, {"_id": 2, "n": 4}, {"_id": 3, "n": 7}]
    out = apply_pipeline(
        docs,
        [{"$densify": {"field": "n", "range": {"bounds": "full", "step": 1}}}],
    )
    assert [d["n"] for d in out] == [1, 2, 3, 4, 5, 6, 7]
    # Original docs keep all fields; fillers have only n.
    [filler] = [d for d in out if d["n"] == 2]
    assert filler == {"n": 2}


def test_densify_explicit_bounds_extends_below_and_above() -> None:
    docs = [{"_id": 1, "n": 5}]
    out = apply_pipeline(
        docs,
        [{"$densify": {"field": "n", "range": {"bounds": [3, 9], "step": 2}}}],
    )
    # Fillers at 3 (below), 7 (above), bounded by [3, 9) — 9 itself excluded.
    assert [d["n"] for d in out] == [3, 5, 7]


def test_densify_step_emits_at_multiples_of_step_strictly_between() -> None:
    docs = [{"_id": 1, "n": 0}, {"_id": 2, "n": 5}]
    out = apply_pipeline(
        docs,
        [{"$densify": {"field": "n", "range": {"bounds": "full", "step": 2}}}],
    )
    # Fillers at 2, 4 between 0 and 5.
    assert [d["n"] for d in out] == [0, 2, 4, 5]


def test_densify_no_input_with_explicit_bounds_emits_full_range() -> None:
    out = apply_pipeline(
        [],
        [{"$densify": {"field": "n", "range": {"bounds": [0, 5], "step": 1}}}],
    )
    assert [d["n"] for d in out] == [0, 1, 2, 3, 4]


def test_densify_no_input_with_full_bounds_returns_empty() -> None:
    """`bounds: 'full'` needs at least one existing doc to derive min/max."""
    out = apply_pipeline(
        [],
        [{"$densify": {"field": "n", "range": {"bounds": "full", "step": 1}}}],
    )
    assert out == []


def test_densify_partitions_independently() -> None:
    docs = [
        {"_id": 1, "g": "a", "n": 1},
        {"_id": 2, "g": "a", "n": 3},
        {"_id": 3, "g": "b", "n": 10},
        {"_id": 4, "g": "b", "n": 12},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$densify": {
                    "field": "n",
                    "partitionByFields": ["g"],
                    "range": {"bounds": "full", "step": 1},
                }
            }
        ],
    )
    by_partition: dict[str, list[int]] = {"a": [], "b": []}
    for d in out:
        by_partition[d["g"]].append(d["n"])
    assert by_partition == {"a": [1, 2, 3], "b": [10, 11, 12]}
    # Filler in partition 'a' carries the partition key.
    [filler_a] = [d for d in out if d["g"] == "a" and d["n"] == 2]
    assert filler_a == {"g": "a", "n": 2}


def test_densify_existing_values_inside_bounds_kept_not_duplicated() -> None:
    docs = [{"_id": 1, "n": 2}, {"_id": 2, "n": 4}]
    out = apply_pipeline(
        docs,
        [{"$densify": {"field": "n", "range": {"bounds": [0, 6], "step": 2}}}],
    )
    assert [d["n"] for d in out] == [0, 2, 4]


def test_densify_invalid_step_raises() -> None:
    with pytest.raises(AggregateError):
        apply_pipeline(
            [{"n": 1}],
            [{"$densify": {"field": "n", "range": {"bounds": "full", "step": 0}}}],
        )


def test_densify_date_unit_day_fills_gaps() -> None:
    """Date densify with ``unit: "day"``: fillers carry the densify
    field as a real ``datetime`` at every step between adjacent
    inputs."""
    import datetime as dt

    docs = [
        {"ts": dt.datetime(2026, 1, 1)},
        {"ts": dt.datetime(2026, 1, 4)},
    ]
    out = apply_pipeline(
        docs,
        [{"$densify": {"field": "ts", "range": {"bounds": "full", "step": 1, "unit": "day"}}}],
    )
    assert [d["ts"] for d in out] == [
        dt.datetime(2026, 1, 1),
        dt.datetime(2026, 1, 2),
        dt.datetime(2026, 1, 3),
        dt.datetime(2026, 1, 4),
    ]


def test_densify_date_unit_hour_with_explicit_bounds() -> None:
    """``unit: "hour"`` + explicit bounds extends below/above the
    observed range, just like the numeric version."""
    import datetime as dt

    docs = [
        {"ts": dt.datetime(2026, 1, 1, 10, 0)},
        {"ts": dt.datetime(2026, 1, 1, 12, 0)},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$densify": {
                    "field": "ts",
                    "range": {
                        "bounds": [dt.datetime(2026, 1, 1, 9, 0), dt.datetime(2026, 1, 1, 13, 0)],
                        "step": 1,
                        "unit": "hour",
                    },
                }
            }
        ],
    )
    assert [d["ts"] for d in out] == [
        dt.datetime(2026, 1, 1, 9, 0),
        dt.datetime(2026, 1, 1, 10, 0),
        dt.datetime(2026, 1, 1, 11, 0),
        dt.datetime(2026, 1, 1, 12, 0),
    ]


def test_densify_date_partitions_independently() -> None:
    """Date densify respects ``partitionByFields`` — each (partition,
    range) pair fills its own gaps and carries the partition values
    onto fillers."""
    import datetime as dt

    docs = [
        {"region": "us", "ts": dt.datetime(2026, 1, 1)},
        {"region": "us", "ts": dt.datetime(2026, 1, 3)},
        {"region": "eu", "ts": dt.datetime(2026, 1, 2)},
        {"region": "eu", "ts": dt.datetime(2026, 1, 4)},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$densify": {
                    "field": "ts",
                    "partitionByFields": ["region"],
                    "range": {"bounds": "full", "step": 1, "unit": "day"},
                }
            }
        ],
    )
    by_region: dict[str, list[object]] = {"us": [], "eu": []}
    for d in out:
        by_region[d["region"]].append(d["ts"])
    assert by_region["us"] == [
        dt.datetime(2026, 1, 1),
        dt.datetime(2026, 1, 2),
        dt.datetime(2026, 1, 3),
    ]
    assert by_region["eu"] == [
        dt.datetime(2026, 1, 2),
        dt.datetime(2026, 1, 3),
        dt.datetime(2026, 1, 4),
    ]


def test_densify_date_unit_month_fills_gaps() -> None:
    """``month`` densify walks via ``relativedelta`` so February's 28/29
    days, October-November's 30-vs-31 days, and the year roll-over all
    handle correctly."""
    import datetime as dt

    docs = [
        {"ts": dt.datetime(2026, 1, 31)},
        {"ts": dt.datetime(2026, 5, 31)},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$densify": {
                    "field": "ts",
                    "range": {"bounds": "full", "step": 1, "unit": "month"},
                }
            }
        ],
    )
    # ``relativedelta`` snaps to the last valid day per month — Jan 31
    # → Feb 28 → Mar 28 → Apr 28 → May 28 (not May 31). The original
    # May-31 doc still appears at the end.
    assert [d["ts"] for d in out] == [
        dt.datetime(2026, 1, 31),
        dt.datetime(2026, 2, 28),
        dt.datetime(2026, 3, 28),
        dt.datetime(2026, 4, 28),
        dt.datetime(2026, 5, 28),
        dt.datetime(2026, 5, 31),
    ]


def test_densify_date_unit_quarter_steps_three_months() -> None:
    import datetime as dt

    docs = [
        {"ts": dt.datetime(2026, 1, 1)},
        {"ts": dt.datetime(2027, 1, 1)},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$densify": {
                    "field": "ts",
                    "range": {"bounds": "full", "step": 1, "unit": "quarter"},
                }
            }
        ],
    )
    assert [d["ts"] for d in out] == [
        dt.datetime(2026, 1, 1),
        dt.datetime(2026, 4, 1),
        dt.datetime(2026, 7, 1),
        dt.datetime(2026, 10, 1),
        dt.datetime(2027, 1, 1),
    ]


def test_densify_date_unit_year_walks_anniversaries() -> None:
    import datetime as dt

    docs = [
        {"ts": dt.datetime(2024, 2, 29)},  # leap day
        {"ts": dt.datetime(2028, 2, 29)},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$densify": {
                    "field": "ts",
                    "range": {"bounds": "full", "step": 1, "unit": "year"},
                }
            }
        ],
    )
    # relativedelta snaps Feb 29 → Feb 28 in non-leap years. Because
    # the walk advances ``cursor + 1 year`` step-by-step (not from the
    # original anchor), once it snaps to Feb 28 it stays at Feb 28
    # on every subsequent step — so 2028's filler is Feb 28 even
    # though 2028 itself is a leap year. The original Feb 29 doc
    # still appears at the end (cursor advances past hi first).
    assert [d["ts"] for d in out] == [
        dt.datetime(2024, 2, 29),
        dt.datetime(2025, 2, 28),
        dt.datetime(2026, 2, 28),
        dt.datetime(2027, 2, 28),
        dt.datetime(2028, 2, 28),
        dt.datetime(2028, 2, 29),
    ]


def test_densify_date_unrecognised_unit_rejected() -> None:
    """Unknown unit string surfaces as a plain ``$densify`` error."""
    import datetime as dt

    with pytest.raises(AggregateError, match="not recognised"):
        apply_pipeline(
            [{"ts": dt.datetime(2026, 1, 1)}],
            [
                {
                    "$densify": {
                        "field": "ts",
                        "range": {"bounds": "full", "step": 1, "unit": "fortnight"},
                    }
                }
            ],
        )


def test_change_stream_stage_stashes_spec_and_returns_empty() -> None:
    """`$changeStream` is a source stage: ignores input, stashes spec on ctx."""
    from secantus.aggregate import PipelineContext, apply_pipeline

    ctx = PipelineContext()
    out = apply_pipeline(
        [{"x": 1}],  # would-be input docs are ignored
        [{"$changeStream": {"fullDocument": "updateLookup"}}],
        ctx,
    )
    assert out == []
    assert ctx.change_stream is not None
    assert ctx.change_stream.full_document_mode == "updateLookup"


# --- $lookup index acceleration --------------------------------------------


def test_lookup_uses_index_when_foreign_field_is_indexed(tmp_path) -> None:
    """When the foreign collection has a single-field index on the
    foreign field, the per-outer-doc lookup goes through Storage's
    index path. Result-equivalence with the hash-join fallback is the
    correctness check; the perf gain isn't asserted (would be flaky).
    """
    from secantus.aggregate import PipelineContext
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert("db", "users", [{"_id": i, "k": i, "name": f"u{i}"} for i in range(200)])
        storage.create_index("db", "users", "k_1", {"k": 1}, {})
        storage.insert("db", "orders", [{"_id": i, "user_k": i % 200} for i in range(50)])

        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_k",
                        "foreignField": "k",
                        "as": "user",
                    }
                }
            ],
            ctx,
        )
        assert len(joined) == 50
        for o in joined:
            assert len(o["user"]) == 1
            assert o["user"][0]["k"] == o["user_k"]
    finally:
        storage.close()


def test_lookup_falls_back_to_hash_join_without_index(tmp_path) -> None:
    """No index → existing hash-join path. Result must match what the
    index path produces."""
    from secantus.aggregate import PipelineContext
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert("db", "users", [{"_id": i, "k": i} for i in range(20)])
        storage.insert("db", "orders", [{"_id": i, "user_k": i % 20} for i in range(10)])

        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_k",
                        "foreignField": "k",
                        "as": "user",
                    }
                }
            ],
            ctx,
        )
        assert len(joined) == 10
        for o in joined:
            assert len(o["user"]) == 1
            assert o["user"][0]["k"] == o["user_k"]
    finally:
        storage.close()


def test_lookup_index_path_handles_array_local_value(tmp_path) -> None:
    """Array local value → matches any element via the indexed $in path."""
    from secantus.aggregate import PipelineContext
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert(
            "db",
            "users",
            [
                {"_id": 1, "k": 100},
                {"_id": 2, "k": 200},
                {"_id": 3, "k": 300},
            ],
        )
        storage.create_index("db", "users", "k_1", {"k": 1}, {})
        storage.insert("db", "orders", [{"_id": 1, "user_ks": [100, 300]}])

        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_ks",
                        "foreignField": "k",
                        "as": "users",
                    }
                }
            ],
            ctx,
        )
        assert len(joined) == 1
        assert sorted(u["k"] for u in joined[0]["users"]) == [100, 300]
    finally:
        storage.close()


def test_lookup_uses_compound_index_when_leading_field_matches(tmp_path) -> None:
    """A compound index whose leading field is the foreign field is
    eligible: each per-outer-doc lookup hits the storage picker as a
    leading-prefix scan, not a hash-join over the materialised foreign
    collection. The path is observable via the eligibility helper —
    `find_matching` doesn't need to be intercepted to verify it."""
    from secantus.aggregate import PipelineContext, _foreign_field_has_simple_index
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert("db", "users", [{"_id": i, "k": i, "tag": "x"} for i in range(10)])
        storage.create_index("db", "users", "k_tag_1", {"k": 1, "tag": 1}, {})

        # The eligibility helper sees the compound index as usable for
        # the leading column.
        assert _foreign_field_has_simple_index(storage, "db", "users", "k") is True
        # And only the leading column — a non-leading column is not
        # equality-indexable on its own.
        assert _foreign_field_has_simple_index(storage, "db", "users", "tag") is False

        storage.insert("db", "orders", [{"_id": i, "user_k": i % 10} for i in range(5)])
        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_k",
                        "foreignField": "k",
                        "as": "user",
                    }
                }
            ],
            ctx,
        )
        assert len(joined) == 5
        for o in joined:
            assert len(o["user"]) == 1
            assert o["user"][0]["k"] == o["user_k"]
    finally:
        storage.close()


def test_lookup_skips_compound_index_when_leading_field_does_not_match(tmp_path) -> None:
    """Compound index whose leading field is *not* the foreign field
    is ineligible — Storage's leading-prefix scan can't handle a
    non-leading equality without a covering single-field index, so the
    `$lookup` falls back to hash-join. Result must still be correct.
    """
    from secantus.aggregate import PipelineContext, _foreign_field_has_simple_index
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert("db", "users", [{"_id": i, "k": i, "tag": f"t{i}"} for i in range(10)])
        # Leading field is `tag`, not `k` — so a `$lookup` on `k` can't
        # use this index.
        storage.create_index("db", "users", "tag_k_1", {"tag": 1, "k": 1}, {})

        assert _foreign_field_has_simple_index(storage, "db", "users", "k") is False
        assert _foreign_field_has_simple_index(storage, "db", "users", "tag") is True

        storage.insert("db", "orders", [{"_id": i, "user_k": i % 10} for i in range(5)])
        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_k",
                        "foreignField": "k",
                        "as": "user",
                    }
                }
            ],
            ctx,
        )
        assert len(joined) == 5
        for o in joined:
            assert len(o["user"]) == 1
            assert o["user"][0]["k"] == o["user_k"]
    finally:
        storage.close()


def test_lookup_uses_multikey_compound_index(tmp_path) -> None:
    """Multikey compound indexes whose leading field matches are now
    eligible — Storage's per-element entries make per-outer-doc IXSCAN
    correct (each foreign array element gets its own entry, so an
    equality lookup still hits all true matches)."""
    from secantus.aggregate import PipelineContext, _foreign_field_has_simple_index, apply_pipeline
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert(
            "db",
            "users",
            [
                {"_id": 1, "k": [1, 2, 3], "tag": "x"},
                {"_id": 2, "k": [3, 4], "tag": "y"},
                {"_id": 3, "k": 5, "tag": "z"},
            ],
        )
        storage.create_index("db", "users", "k_tag_1", {"k": 1, "tag": 1}, {})
        assert _foreign_field_has_simple_index(storage, "db", "users", "k") is True

        storage.insert("db", "orders", [{"_id": 10, "user_k": 3}, {"_id": 11, "user_k": 5}])
        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_k",
                        "foreignField": "k",
                        "as": "user",
                    }
                }
            ],
            ctx,
        )
        # Order 10 (user_k=3) matches users 1 and 2.
        # Order 11 (user_k=5) matches user 3.
        joined.sort(key=lambda d: d["_id"])
        assert sorted(u["_id"] for u in joined[0]["user"]) == [1, 2]
        assert [u["_id"] for u in joined[1]["user"]] == [3]
    finally:
        storage.close()


def test_lookup_pipeline_form_uses_index_when_available(tmp_path) -> None:
    """Simple-form-plus-pipeline also picks up the index for the
    pre-filter; the sub-pipeline still runs on the narrowed candidate
    set. Verifies parity with the hash-join path."""
    from secantus.aggregate import PipelineContext
    from secantus.storage import Storage

    storage = Storage(str(tmp_path))
    try:
        storage.insert(
            "db",
            "users",
            [{"_id": i, "k": i, "active": i % 2 == 0} for i in range(20)],
        )
        storage.create_index("db", "users", "k_1", {"k": 1}, {})
        storage.insert("db", "orders", [{"_id": i, "user_k": i % 20} for i in range(10)])

        ctx = PipelineContext(storage=storage, db_name="db")
        outer = list(storage.find_matching("db", "orders", {}))
        joined = apply_pipeline(
            outer,
            [
                {
                    "$lookup": {
                        "from": "users",
                        "localField": "user_k",
                        "foreignField": "k",
                        "let": {"o_id": "$_id"},
                        "pipeline": [{"$match": {"active": True}}],
                        "as": "active_user",
                    }
                }
            ],
            ctx,
        )
        assert len(joined) == 10
        for o in joined:
            if o["user_k"] % 2 == 0:
                assert len(o["active_user"]) == 1
                assert o["active_user"][0]["active"] is True
            else:
                assert o["active_user"] == []
    finally:
        storage.close()


# ----------------------------------------------------------------------
# $fill (5.3+): replace missing / null fields in-place. Three modes:
# {value: <expr>}, {method: "locf"}, {method: "linear"}, optionally
# partitioned and sorted.


def test_fill_value_replaces_missing_field() -> None:
    docs = [{"_id": 1, "n": 5}, {"_id": 2}, {"_id": 3, "n": None}]
    out = apply_pipeline(docs, [{"$fill": {"output": {"n": {"value": 0}}}}])
    assert [d["n"] for d in out] == [5, 0, 0]


def test_fill_value_leaves_explicit_zero_alone() -> None:
    """Only missing / null trigger the fill — explicit 0, False, "" are kept."""
    docs = [{"_id": 1, "n": 0}, {"_id": 2, "n": False}, {"_id": 3, "n": ""}]
    out = apply_pipeline(docs, [{"$fill": {"output": {"n": {"value": 99}}}}])
    assert [d["n"] for d in out] == [0, False, ""]


def test_fill_value_expression_evaluated_per_doc() -> None:
    """value: <expr> can reference other fields of the current doc."""
    docs = [{"_id": 1, "x": 10}, {"_id": 2, "x": 20}, {"_id": 3, "x": 30, "n": 7}]
    out = apply_pipeline(docs, [{"$fill": {"output": {"n": {"value": "$x"}}}}])
    assert [d["n"] for d in out] == [10, 20, 7]


def test_fill_locf_carries_last_observation_forward() -> None:
    docs = [
        {"_id": 1, "t": 1, "v": 10},
        {"_id": 2, "t": 2},
        {"_id": 3, "t": 3, "v": None},
        {"_id": 4, "t": 4, "v": 20},
        {"_id": 5, "t": 5},
    ]
    out = apply_pipeline(
        docs,
        [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "locf"}}}}],
    )
    assert [d["v"] for d in out] == [10, 10, 10, 20, 20]


def test_fill_locf_leading_nulls_stay_null() -> None:
    """LOCF can't fill before the first observed value — leaves nulls."""
    docs = [
        {"_id": 1, "t": 1},
        {"_id": 2, "t": 2},
        {"_id": 3, "t": 3, "v": 100},
        {"_id": 4, "t": 4},
    ]
    out = apply_pipeline(
        docs,
        [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "locf"}}}}],
    )
    assert [d.get("v") for d in out] == [None, None, 100, 100]


def test_fill_linear_interpolates_between_anchors() -> None:
    docs = [
        {"_id": 1, "t": 0, "v": 10},
        {"_id": 2, "t": 1},
        {"_id": 3, "t": 2},
        {"_id": 4, "t": 3, "v": 40},
    ]
    out = apply_pipeline(
        docs,
        [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "linear"}}}}],
    )
    assert [d["v"] for d in out] == [10, 20, 30, 40]


def test_fill_linear_leaves_trailing_nulls() -> None:
    """Linear needs two anchors; trailing nulls after the last anchor stay null."""
    docs = [
        {"_id": 1, "t": 0, "v": 10},
        {"_id": 2, "t": 1, "v": 20},
        {"_id": 3, "t": 2},
        {"_id": 4, "t": 3},
    ]
    out = apply_pipeline(
        docs,
        [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "linear"}}}}],
    )
    assert out[0]["v"] == 10
    assert out[1]["v"] == 20
    assert out[2].get("v") is None
    assert out[3].get("v") is None


def test_fill_partition_keeps_groups_independent() -> None:
    """LOCF inside one partition doesn't leak across partitions."""
    docs = [
        {"_id": 1, "p": "a", "t": 1, "v": 1},
        {"_id": 2, "p": "a", "t": 2},
        {"_id": 3, "p": "b", "t": 1, "v": 100},
        {"_id": 4, "p": "b", "t": 2},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$fill": {
                    "partitionByFields": ["p"],
                    "sortBy": {"t": 1},
                    "output": {"v": {"method": "locf"}},
                }
            }
        ],
    )
    by_id = {d["_id"]: d for d in out}
    assert by_id[1]["v"] == 1
    assert by_id[2]["v"] == 1
    assert by_id[3]["v"] == 100
    assert by_id[4]["v"] == 100


def test_fill_method_without_sortby_rejected() -> None:
    docs = [{"_id": 1, "v": 1}, {"_id": 2}]
    with pytest.raises(AggregateError):
        apply_pipeline(docs, [{"$fill": {"output": {"v": {"method": "locf"}}}}])


def test_fill_rejects_partitionby_and_partitionbyfields_together() -> None:
    docs = [{"_id": 1, "v": 1}]
    with pytest.raises(AggregateError):
        apply_pipeline(
            docs,
            [
                {
                    "$fill": {
                        "partitionBy": "$p",
                        "partitionByFields": ["p"],
                        "output": {"v": {"value": 0}},
                    }
                }
            ],
        )


def test_fill_rejects_unknown_method() -> None:
    docs = [{"_id": 1, "v": 1}]
    with pytest.raises(AggregateError):
        apply_pipeline(
            docs,
            [
                {
                    "$fill": {
                        "sortBy": {"t": 1},
                        "output": {"v": {"method": "bogus"}},
                    }
                }
            ],
        )


def test_fill_value_and_method_combine_within_output() -> None:
    """Multiple output fields can mix modes; locf needs sortBy, value doesn't."""
    docs = [
        {"_id": 1, "t": 1, "tag": "A", "n": 10},
        {"_id": 2, "t": 2},
        {"_id": 3, "t": 3, "tag": "C"},
    ]
    out = apply_pipeline(
        docs,
        [
            {
                "$fill": {
                    "sortBy": {"t": 1},
                    "output": {
                        "n": {"method": "locf"},
                        "tag": {"value": "X"},
                    },
                }
            }
        ],
    )
    assert [(d["t"], d["n"], d["tag"]) for d in out] == [
        (1, 10, "A"),
        (2, 10, "X"),
        (3, 10, "C"),
    ]


def test_fill_linear_dates_interpolate_via_timedelta() -> None:
    """Date interpolation: timedelta arithmetic gives float / timedelta."""
    import datetime as dt

    base = dt.datetime(2026, 1, 1)
    docs = [
        {"_id": 1, "t": base, "v": 0},
        {"_id": 2, "t": base + dt.timedelta(days=1)},
        {"_id": 3, "t": base + dt.timedelta(days=2)},
        {"_id": 4, "t": base + dt.timedelta(days=3), "v": 30},
    ]
    out = apply_pipeline(
        docs,
        [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "linear"}}}}],
    )
    assert [d["v"] for d in out] == [0, 10, 20, 30]


def test_now_system_variable() -> None:
    """$$NOW is a Date constant across the pipeline (mongod semantics)."""
    import datetime

    docs = [{"_id": 1}, {"_id": 2}]
    out = apply_pipeline(docs, [{"$addFields": {"t": "$$NOW"}}])
    assert all(isinstance(d["t"], datetime.datetime) for d in out)
    assert out[0]["t"] == out[1]["t"]
    now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
    assert abs((now - out[0]["t"].replace(tzinfo=None)).total_seconds()) < 60
