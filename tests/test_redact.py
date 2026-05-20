"""``$redact`` — content-based document / sub-document pruning.

The stage's expression is evaluated against each (sub-)document and
must return one of three sentinel strings:

* ``"$$KEEP"`` — include the sub-doc as-is, no recursion.
* ``"$$PRUNE"`` — drop the sub-doc (top-level drops the doc from
  the pipeline; nested drops it from the parent's field / array).
* ``"$$DESCEND"`` — recurse into every dict-valued field and every
  dict-valued list element.

Mongod uses ``$redact`` to implement document-level access control
("docs tagged confidential land in the pipeline filtered out"),
which we pin here at the operator-semantics level rather than via
RBAC.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


# ---------------------------------------------------------------------------
# Top-level decisions: KEEP / PRUNE
# ---------------------------------------------------------------------------


def test_redact_keep_returns_doc_unchanged(client) -> None:
    """Expression unconditionally returns $$KEEP → every doc passes
    through with no modification."""
    coll = client["redact_db"]["keep"]
    coll.insert_many([{"_id": 1, "v": 10}, {"_id": 2, "v": 20}])

    pipeline = [{"$redact": "$$KEEP"}]
    docs = sorted(coll.aggregate(pipeline), key=lambda d: d["_id"])
    assert docs == [{"_id": 1, "v": 10}, {"_id": 2, "v": 20}]


def test_redact_prune_drops_all_docs(client) -> None:
    """Expression unconditionally returns $$PRUNE → empty result."""
    coll = client["redact_db"]["prune_all"]
    coll.insert_many([{"_id": 1}, {"_id": 2}])

    pipeline = [{"$redact": "$$PRUNE"}]
    assert list(coll.aggregate(pipeline)) == []


def test_redact_conditional_keep_or_prune(client) -> None:
    """Top-level decision conditional on a doc field — the access-control
    canon. Docs tagged ``visible: false`` are dropped."""
    coll = client["redact_db"]["cond"]
    coll.insert_many(
        [
            {"_id": 1, "v": 10, "visible": True},
            {"_id": 2, "v": 20, "visible": False},
            {"_id": 3, "v": 30, "visible": True},
        ]
    )
    pipeline = [{"$redact": {"$cond": {"if": "$visible", "then": "$$KEEP", "else": "$$PRUNE"}}}]
    docs = sorted(coll.aggregate(pipeline), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 3]


# ---------------------------------------------------------------------------
# $$DESCEND: nested sub-docs
# ---------------------------------------------------------------------------


def test_descend_into_nested_subdocs_prunes_tagged(client) -> None:
    """$$DESCEND at every level + $$PRUNE for sub-docs flagged
    classified. The doc itself stays; the classified sub-doc is gone."""
    coll = client["redact_db"]["descend_subdoc"]
    coll.insert_one(
        {
            "_id": 1,
            "name": "outer",
            "details": {
                "name": "inner",
                "classified": True,
                "value": "secret",
            },
            "other": "kept",
        }
    )
    pipeline = [
        {
            "$redact": {
                "$cond": {
                    "if": {"$eq": [{"$ifNull": ["$classified", False]}, True]},
                    "then": "$$PRUNE",
                    "else": "$$DESCEND",
                }
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    assert len(docs) == 1
    assert docs[0]["_id"] == 1
    assert docs[0]["name"] == "outer"
    assert docs[0]["other"] == "kept"
    assert "details" not in docs[0]  # pruned


def test_descend_into_arrays_of_subdocs(client) -> None:
    """Arrays of sub-docs: each element with ``hidden: true`` is removed,
    others stay. Non-dict elements pass through unchanged."""
    coll = client["redact_db"]["descend_array"]
    coll.insert_one(
        {
            "_id": 1,
            "items": [
                {"name": "a", "hidden": False},
                {"name": "b", "hidden": True},
                {"name": "c"},
                "literal",  # non-dict — should pass through
                42,
            ],
        }
    )
    pipeline = [
        {
            "$redact": {
                "$cond": {
                    "if": {"$eq": [{"$ifNull": ["$hidden", False]}, True]},
                    "then": "$$PRUNE",
                    "else": "$$DESCEND",
                }
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    assert len(docs) == 1
    items = docs[0]["items"]
    # b was hidden (pruned). a, c kept. Non-dict elements stay.
    assert [i.get("name") if isinstance(i, dict) else i for i in items] == [
        "a",
        "c",
        "literal",
        42,
    ]


def test_descend_recurses_multiple_levels(client) -> None:
    """Recursion must walk N levels deep, not just one. Top-level keeps,
    middle sub-doc keeps, leaf sub-doc with ``secret: true`` is pruned."""
    coll = client["redact_db"]["deep"]
    coll.insert_one(
        {
            "_id": 1,
            "level1": {
                "name": "L1",
                "level2": {
                    "name": "L2",
                    "level3": {"name": "L3", "secret": True},
                    "side": {"name": "side-L3"},
                },
            },
        }
    )
    pipeline = [
        {
            "$redact": {
                "$cond": {
                    "if": {"$eq": [{"$ifNull": ["$secret", False]}, True]},
                    "then": "$$PRUNE",
                    "else": "$$DESCEND",
                }
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    assert docs[0]["level1"]["level2"]["side"]["name"] == "side-L3"
    assert "level3" not in docs[0]["level1"]["level2"]


# ---------------------------------------------------------------------------
# $$KEEP short-circuits descent
# ---------------------------------------------------------------------------


def test_keep_does_not_recurse(client) -> None:
    """A sub-doc that returns $$KEEP is included unchanged — recursion
    stops there. A nested $$PRUNE underneath is silently ignored."""
    coll = client["redact_db"]["keep_no_recurse"]
    coll.insert_one(
        {
            "_id": 1,
            "outer": {
                "trusted": True,  # this sub-doc returns $$KEEP
                "inner": {"secret": True, "data": "exposed"},
            },
        }
    )
    pipeline = [
        {
            "$redact": {
                "$switch": {
                    "branches": [
                        {"case": {"$eq": ["$trusted", True]}, "then": "$$KEEP"},
                        {"case": {"$eq": ["$secret", True]}, "then": "$$PRUNE"},
                    ],
                    "default": "$$DESCEND",
                }
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    # Because outer returned $$KEEP, the inner doc's $$PRUNE never fires.
    assert docs[0]["outer"]["inner"] == {"secret": True, "data": "exposed"}


# ---------------------------------------------------------------------------
# Edge cases / errors
# ---------------------------------------------------------------------------


def test_redact_non_sentinel_return_rejected(client) -> None:
    """A non-sentinel string fails the dispatcher."""
    coll = client["redact_db"]["bad"]
    coll.insert_one({"_id": 1})

    with pytest.raises(OperationFailure):
        list(coll.aggregate([{"$redact": "not a sentinel"}]))


def test_redact_missing_expression_rejected(client) -> None:
    coll = client["redact_db"]["missing"]
    coll.insert_one({"_id": 1})

    with pytest.raises(OperationFailure):
        list(coll.aggregate([{"$redact": None}]))

    with pytest.raises(OperationFailure):
        list(coll.aggregate([{"$redact": {}}]))


def test_redact_followed_by_match(client) -> None:
    """A redact stage feeding into $match — typical pipeline shape.
    Confirms the returned docs round-trip through subsequent stages."""
    coll = client["redact_db"]["chain"]
    coll.insert_many(
        [
            {"_id": 1, "v": 1, "visible": True},
            {"_id": 2, "v": 2, "visible": False},
            {"_id": 3, "v": 3, "visible": True},
        ]
    )
    pipeline = [
        {"$redact": {"$cond": {"if": "$visible", "then": "$$KEEP", "else": "$$PRUNE"}}},
        {"$match": {"v": {"$gt": 1}}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert [d["_id"] for d in docs] == [3]


def test_redact_keep_inside_array_keeps_subdoc_unchanged(client) -> None:
    """An array-element sub-doc whose redact result is $$KEEP includes
    the whole sub-doc unchanged, including any nested sub-docs that
    would otherwise be pruned."""
    coll = client["redact_db"]["array_keep"]
    coll.insert_one(
        {
            "_id": 1,
            "items": [
                {
                    "name": "a",
                    "trusted": True,
                    "inner": {"secret": True, "data": "kept-anyway"},
                },
                {
                    "name": "b",
                    "trusted": False,
                    "inner": {"secret": True, "data": "should-not-survive"},
                },
            ],
        }
    )
    pipeline = [
        {
            "$redact": {
                "$switch": {
                    "branches": [
                        {"case": {"$eq": ["$trusted", True]}, "then": "$$KEEP"},
                        {"case": {"$eq": ["$secret", True]}, "then": "$$PRUNE"},
                    ],
                    "default": "$$DESCEND",
                }
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    items = docs[0]["items"]
    # "a" returned $$KEEP → whole sub-doc preserved with nested secret.
    a = next(i for i in items if i.get("name") == "a")
    assert a["inner"]["secret"] is True
    # "b" descended → its inner has secret=True → pruned.
    b = next(i for i in items if i.get("name") == "b")
    assert "inner" not in b
