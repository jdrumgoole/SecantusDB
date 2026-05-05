from __future__ import annotations

import pytest

from secantus.aggregate import AggregateError, apply_pipeline


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
