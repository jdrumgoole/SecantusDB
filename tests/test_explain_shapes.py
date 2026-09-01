"""``explain``'s normalised query and stage tree.

Every expectation here is mongod 8.2.11's actual output, measured on
2026-09-01 with ``tools/probes/explain_shapes.py`` (56 filters and 25 find
shapes, all agreeing at the time this file was written). The probe is the
exploratory front end; this file is the standing gate that does not need a
mongod on the box.

Two things get pinned:

* ``canonical_match`` -- mongod echoes the ``MatchExpression`` tree AFTER
  normalisation, not the filter you sent. The child ORDER inside ``$and`` is the
  part with no documentation behind it: it is mongod's internal type ordinal,
  derived pairwise (``PROBE_ORDER=1`` on the probe).
* ``build_stage_tree`` -- the SORT / SKIP / LIMIT / PROJECTION_* nesting, whose
  practical value is that a blocking ``SORT`` is what tells a client its sort is
  NOT served by an index.
"""

from __future__ import annotations

import pytest
from bson import SON

from secantus.explain import build_stage_tree, canonical_match, projection_stage_name


@pytest.mark.parametrize(
    ("filter_", "expected"),
    [
        ({}, {}),
        ({"a": 1}, {"a": {"$eq": 1}}),
        ({"a": None}, {"a": {"$eq": None}}),
        ({"a": {"$eq": 5}}, {"a": {"$eq": 5}}),
        ({"sub": {"k": 1}}, {"sub": {"$eq": {"k": 1}}}),
        ({"a": {"$ne": 3}}, {"a": {"$not": {"$eq": 3}}}),
        ({"a": {"$nin": [1, 2]}}, {"a": {"$not": {"$in": [1, 2]}}}),
        ({"a": {"$in": [1]}}, {"a": {"$eq": 1}}),
        ({"a": {"$in": []}}, {"$alwaysFalse": 1}),
        ({"a": {"$all": [1]}}, {"a": {"$eq": 1}}),
        (
            {"a": {"$all": [{"$elemMatch": {"x": 1}}]}},
            {"a": {"$elemMatch": {"x": {"$eq": 1}}}},
        ),
        ({"a": {"$type": "int"}}, {"a": {"$type": [16]}}),
        ({"a": {"$type": 2}}, {"a": {"$type": [2]}}),
        ({"a": {"$type": ["string", "int"]}}, {"a": {"$type": [2, 16]}}),
        # "number" has no single BSON code, so mongod keeps the alias.
        ({"a": {"$type": "number"}}, {"a": {"$type": ["number"]}}),
        ({"a": {"$bitsAllSet": 1}}, {"a": {"$bitsAllSet": [0]}}),
        ({"a": {"$regex": "^x"}}, {"a": {"$regex": "^x"}}),
        (
            {"a": {"$regex": "^x", "$options": "i"}},
            {"a": {"$regex": "^x", "$options": "i"}},
        ),
        ({"a": {"$not": {"$gt": 1}}}, {"a": {"$not": {"$gt": 1}}}),
        ({"a": {"$elemMatch": {"$gt": 1}}}, {"a": {"$elemMatch": {"$gt": 1}}}),
        ({"$and": [{"a": 1}]}, {"a": {"$eq": 1}}),
        ({"$or": [{"a": 1}]}, {"a": {"$eq": 1}}),
        ({"$nor": [{"a": 1}]}, {"a": {"$not": {"$eq": 1}}}),
        (SON([("a", 1), ("$comment", "hi")]), {"a": {"$eq": 1}}),
    ],
)
def test_canonical_match_single_clause(filter_, expected):
    assert canonical_match(filter_) == expected


def test_and_children_sort_by_type_then_path():
    """``{a: {$gt: 1}, b: 2}`` reports ``b``'s equality FIRST -- the match type
    dominates the path, which is why input order tells you nothing."""
    assert canonical_match(SON([("a", {"$gt": 1}), ("b", 2)])) == {
        "$and": [{"b": {"$eq": 2}}, {"a": {"$gt": 1}}]
    }
    # Same type: the path breaks the tie, whatever order they arrived in.
    assert canonical_match(SON([("b", 2), ("a", 1)])) == {
        "$and": [{"a": {"$eq": 1}}, {"b": {"$eq": 2}}]
    }


def test_operators_on_one_path_split_into_ordered_and_children():
    assert canonical_match({"a": SON([("$gt", 3), ("$lt", 9)])}) == {
        "$and": [{"a": {"$lt": 9}}, {"a": {"$gt": 3}}]
    }
    assert canonical_match({"a": SON([("$gt", 1), ("$lt", 9), ("$ne", 5)])}) == {
        "$and": [{"a": {"$lt": 9}}, {"a": {"$gt": 1}}, {"a": {"$not": {"$eq": 5}}}]
    }


def test_nested_and_flattens():
    assert canonical_match({"$and": [{"$and": [{"a": 1}]}, {"b": 2}]}) == {
        "$and": [{"a": {"$eq": 1}}, {"b": {"$eq": 2}}]
    }


def test_or_sorts_ahead_of_field_clauses():
    got = canonical_match(SON([("$or", [{"b": 1}, {"c": 2}]), ("a", 1)]))
    assert got == {
        "$and": [
            {"$or": [{"b": {"$eq": 1}}, {"c": {"$eq": 2}}]},
            {"a": {"$eq": 1}},
        ]
    }


def test_nor_survives_alone_and_decomposes_inside_an_and():
    """A ``$nor`` is a node only while it is the whole query."""
    assert canonical_match({"$nor": [{"a": 1}, {"b": 2}]}) == {
        "$nor": [{"a": {"$eq": 1}}, {"b": {"$eq": 2}}]
    }
    assert canonical_match(SON([("$nor", [{"b": 1}, {"c": 2}]), ("a", 1)])) == {
        "$and": [
            {"a": {"$eq": 1}},
            {"b": {"$not": {"$eq": 1}}},
            {"c": {"$not": {"$eq": 2}}},
        ]
    }


def test_elem_match_object_form_is_canonicalised_inside():
    assert canonical_match({"a": {"$elemMatch": SON([("x", 1), ("y", {"$gt": 2})])}}) == {
        "a": {"$elemMatch": {"$and": [{"x": {"$eq": 1}}, {"y": {"$gt": 2}}]}}
    }


@pytest.mark.parametrize(
    ("projection", "stage"),
    [
        ({"a": 1}, "PROJECTION_SIMPLE"),
        ({"a": 0}, "PROJECTION_SIMPLE"),
        ({"_id": 0, "a": 1}, "PROJECTION_SIMPLE"),
        ({"a.b": 1}, "PROJECTION_DEFAULT"),
        ({"a": {"$elemMatch": {"x": 1}}}, "PROJECTION_DEFAULT"),
        ({"a": {"$slice": 2}}, "PROJECTION_DEFAULT"),
    ],
)
def test_projection_stage_name(projection, stage):
    assert projection_stage_name(projection) == stage


SCAN = {"stage": "COLLSCAN", "direction": "forward"}


def _chain(node):
    out = []
    while isinstance(node, dict) and "stage" in node:
        out.append(node["stage"])
        node = node.get("inputStage")
    return out


def _tree(**kw):
    kw.setdefault("sort", None)
    kw.setdefault("sort_served_by_index", False)
    kw.setdefault("projection", None)
    kw.setdefault("skip", None)
    kw.setdefault("limit", None)
    return build_stage_tree(dict(SCAN), **kw)


def test_bare_scan_is_not_wrapped():
    assert _tree() == SCAN


def test_limit_is_outermost_and_skip_innermost():
    assert _chain(_tree(skip=2, limit=3, projection={"a": 1})) == [
        "LIMIT",
        "PROJECTION_SIMPLE",
        "SKIP",
        "COLLSCAN",
    ]


def test_a_blocking_sort_absorbs_the_limit():
    """No ``LIMIT`` stage appears, and the sort must retain what the skip will
    later drop -- so ``limitAmount`` is limit PLUS skip."""
    tree = _tree(sort={"zzz": 1}, skip=2, limit=3, projection={"a": 1})
    assert _chain(tree) == ["PROJECTION_SIMPLE", "SKIP", "SORT", "COLLSCAN"]
    sort_stage = tree["inputStage"]["inputStage"]
    assert sort_stage["limitAmount"] == 5
    assert sort_stage["sortPattern"] == {"zzz": 1}
    assert sort_stage["type"] == "simple"


def test_an_index_served_sort_emits_no_sort_stage():
    """This is the question a client runs explain to answer."""
    assert _chain(_tree(sort={"a": 1}, sort_served_by_index=True, limit=3)) == [
        "LIMIT",
        "COLLSCAN",
    ]


def test_a_negative_limit_reports_its_magnitude():
    """pymongo sends a negative limit to mean "one batch", not a smaller one."""
    tree = _tree(limit=-3)
    assert tree["stage"] == "LIMIT"
    assert tree["limitAmount"] == 3
