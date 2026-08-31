"""``$redact`` — content-based document / sub-document pruning.

The stage's expression is evaluated against each (sub-)document and
must return one of three decision VARIABLES -- not the strings that
name them, which is the distinction three of the tests below exist for:

* ``$$KEEP`` -- include the sub-doc as-is, no recursion.
* ``$$PRUNE`` -- drop the sub-doc (top-level drops the doc from
  the pipeline; nested drops it from the parent's field / array).
* ``$$DESCEND`` -- recurse into every dict-valued field, every
  dict-valued list element, and every NESTED list.

mongod binds these three names only while evaluating a ``$redact``
expression; anywhere else they are undefined variables (17276).

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


# Module-scoped: one server for the file, with `_fresh_databases` below
# giving each test the clean slate a per-test server used to.
@pytest.fixture(scope="module")
def server(wt_home_module):
    with SecantusDBServer(port=0, storage_path=wt_home_module) as srv:
        yield srv


@pytest.fixture(autouse=True)
def _fresh_databases(client):
    """Drop everything this test made, so the shared server looks new to the next.

    The isolation a per-test server gave for free, without paying for a server.
    Runs AFTER the test so a failure leaves its data in place for inspection.
    """
    yield
    for _name in client.list_database_names():
        if _name not in ("admin", "local", "config"):
            client.drop_database(_name)


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


# --- the three bugs found by probing 8.2.11 on 2026-08-31 ------------------
#
# All three were present on BOTH servers, and all three made `$redact` return
# data it exists to withhold. They are pinned separately because they fail
# independently.


def test_a_stored_string_cannot_impersonate_a_decision(client) -> None:
    """A field whose VALUE is ``"$$KEEP"`` is not the ``$$KEEP`` variable.

    The stage used to compare the evaluated result against the string
    ``"$$KEEP"``, so ``$redact: "$tag"`` over caller-controlled content kept a
    document mongod refuses to keep -- disclosure driven by document content,
    from the stage whose whole job is to withhold it.
    """
    coll = client["redact_db"]["impersonate"]
    coll.insert_one({"_id": 1, "tag": "$$KEEP", "secret": "should-not-survive"})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$redact": "$tag"}]))
    assert exc.value.code == 17053
    assert "should not return anything aside from the variables" in str(exc.value)


def test_redact_descends_into_nested_arrays(client) -> None:
    """A sub-doc one array deeper used to be passed through untouched.

    mongod prunes it and leaves the (now empty) inner array in place.
    """
    coll = client["redact_db"]["nested_arrays"]
    coll.insert_one({"_id": 1, "lvl": 1, "n": [[{"lvl": 9, "x": 1}], {"lvl": 1, "y": 2}]})
    pipeline = [
        {"$redact": {"$cond": [{"$lte": [{"$ifNull": ["$lvl", 0]}, 3]}, "$$DESCEND", "$$PRUNE"]}}
    ]
    assert list(coll.aggregate(pipeline)) == [{"_id": 1, "lvl": 1, "n": [[], {"lvl": 1, "y": 2}]}]


def test_the_decision_variables_are_undefined_outside_redact(client) -> None:
    """``$project: {x: "$$KEEP"}`` used to return the marker string as data."""
    coll = client["redact_db"]["outside"]
    coll.insert_one({"_id": 1, "a": 1})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"x": "$$KEEP"}}]))
    assert exc.value.code == 17276
    assert "Use of undefined variable: KEEP" in str(exc.value)


@pytest.mark.parametrize(
    ("spec", "rendered"),
    [
        (5, "5"),
        ("x", '"x"'),
        (True, "true"),
        (None, "null"),
        ({}, "{}"),
        ({"$literal": {"k": 1, "j": "s"}}, '{k: 1, j: "s"}'),
        ({"$literal": [1, "a"]}, '[1, "a"]'),
    ],
)
def test_a_non_decision_result_is_17053_rendered_mongods_way(client, spec, rendered) -> None:
    """mongod's compact ``Value::toString``: no inner spaces in containers.

    That is a DIFFERENT renderer from the shell form other messages use
    (``{ k: 1 }`` with spaces, ``ObjectId('...')``); both are mongod's.
    """
    coll = client["redact_db"]["render"]
    coll.insert_one({"_id": 1, "a": 1})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$redact": spec}]))
    assert exc.value.code == 17053
    assert str(exc.value).split("but returned ")[1].startswith(rendered)
