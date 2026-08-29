"""`$lookup` / `$graphLookup` match mongod's joins and its argument errors.

Phase 2 of ``tasks/remaining-work-plan.md``, fourth surface: 27 shapes probed
against a live mongod 6.0.16, of which 20 diverged.

The one that mattered most is a **truncated traversal**: ``$graphLookup``
stopped following the chain the moment a ``connectFromField`` was null, so a
four-document chain came back with one document in it. Nothing errored -- the
answer was simply short, which is the hardest kind of wrong to notice.

A correction worth recording, because it shaped how this was scoped: the stage
was first reported as "does not recurse at all". It does. The original fixture
happened to put a null link on the very first hop, so one bug looked like a
missing feature. Probing a chain with no nulls showed the traversal, the
``maxDepth`` cut-off and an array ``startWith`` all working.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def db(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli["lk"]
    finally:
        cli.close()
        srv.stop()


def _seed(db, orders, stock):
    db.orders.drop()
    db.stock.drop()
    db.orders.insert_many([dict(d) for d in orders])
    db.stock.insert_many([dict(d) for d in stock])


def _err(db, pipeline):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(db.orders.aggregate(pipeline))
    return exc.value


# --- $graphLookup: the truncated traversal ----------------------------------

CHAIN = [
    {"_id": 10, "sku": "a", "parent": None},
    {"_id": 11, "sku": "b", "parent": "a"},
    {"_id": 12, "sku": "c", "parent": "b"},
    {"_id": 13, "sku": None, "parent": "c"},
]

GRAPH = {
    "from": "stock",
    "startWith": "$sku",
    "connectFromField": "parent",
    "connectToField": "sku",
    "as": "chain",
}


def test_a_null_link_does_not_end_the_traversal(db) -> None:
    """The regression: the walk stopped at the first null ``parent``, so this
    four-document chain returned one document and no error."""
    _seed(db, [{"_id": 1, "sku": "a"}], CHAIN)
    got = next(iter(db.orders.aggregate([{"$graphLookup": GRAPH}])))
    assert sorted(d["_id"] for d in got["chain"]) == [10, 11, 12, 13]


def test_a_missing_link_does_end_the_traversal(db) -> None:
    """Missing and null are different: an ABSENT ``connectFromField`` stops the
    walk, an explicit null does not (probed both ways on 6.0.16)."""
    _seed(db, [{"_id": 1, "sku": "a"}], [{"_id": 10, "sku": "a"}, {"_id": 11, "sku": None}])
    got = next(iter(db.orders.aggregate([{"$graphLookup": GRAPH}])))
    assert [d["_id"] for d in got["chain"]] == [10]


def test_a_null_link_does_not_reach_a_document_without_the_field(db) -> None:
    """A document that simply lacks ``connectToField`` is not null -- it is
    absent, and a null link must not match it. Comparing ``get_path``'s None
    for both made every field-less document reachable."""
    _seed(db, [{"_id": 1, "sku": "a"}], [{"_id": 10, "sku": "a", "parent": None}, {"_id": 11}])
    got = next(iter(db.orders.aggregate([{"$graphLookup": GRAPH}])))
    assert [d["_id"] for d in got["chain"]] == [10]


def test_a_chain_with_no_nulls_still_walks(db) -> None:
    """The behaviour that was NOT broken, pinned so the fix can't over-reach."""
    _seed(
        db,
        [{"_id": 1, "sku": "a"}],
        [
            {"_id": 1, "sku": "a", "parent": "b"},
            {"_id": 2, "sku": "b", "parent": "c"},
            {"_id": 3, "sku": "c", "parent": "d"},
            {"_id": 4, "sku": "d", "parent": "end"},
        ],
    )
    got = next(iter(db.orders.aggregate([{"$graphLookup": GRAPH}])))
    assert sorted(d["_id"] for d in got["chain"]) == [1, 2, 3, 4]


def test_maxdepth_still_cuts_the_walk(db) -> None:
    _seed(
        db,
        [{"_id": 1, "sku": "a"}],
        [
            {"_id": 1, "sku": "a", "parent": "b"},
            {"_id": 2, "sku": "b", "parent": "c"},
            {"_id": 3, "sku": "c", "parent": "d"},
        ],
    )
    got = next(iter(db.orders.aggregate([{"$graphLookup": {**GRAPH, "maxDepth": 1}}])))
    assert sorted(d["_id"] for d in got["chain"]) == [1, 2]


# --- $graphLookup argument errors -------------------------------------------


def test_negative_maxdepth_is_rejected(db) -> None:
    """It was accepted and matched NOTHING -- every document got an empty
    array, which reads as "no connections" rather than "bad option"."""
    _seed(db, [{"_id": 1, "sku": "a"}], CHAIN)
    err = _err(db, [{"$graphLookup": {**GRAPH, "maxDepth": -1}}])
    assert err.code == 40101
    assert "maxDepth requires a nonnegative argument, found: -1" in str(err)


def test_unknown_graphlookup_argument_is_rejected(db) -> None:
    _seed(db, [{"_id": 1, "sku": "a"}], CHAIN)
    err = _err(db, [{"$graphLookup": {**GRAPH, "zz": 1}}])
    assert err.code == 40104
    assert "Unknown argument to $graphLookup: zz" in str(err)


def test_graphlookup_missing_required_argument(db) -> None:
    _seed(db, [{"_id": 1, "sku": "a"}], CHAIN)
    spec = {k: v for k, v in GRAPH.items() if k != "as"}
    err = _err(db, [{"$graphLookup": spec}])
    assert err.code == 40105


def test_graphlookup_spec_must_be_a_document(db) -> None:
    _seed(db, [{"_id": 1, "sku": "a"}], CHAIN)
    err = _err(db, [{"$graphLookup": 5}])
    assert err.code == 9
    assert "must be an object, but found int" in str(err)


# --- $lookup: the crashes ----------------------------------------------------

ORDERS = [{"_id": 1, "sku": "a"}, {"_id": 2, "sku": "b"}]
STOCK = [{"_id": 10, "sku": "a"}, {"_id": 11, "sku": None}]
JOIN = {"from": "stock", "localField": "sku", "foreignField": "sku", "as": "s"}


@pytest.mark.parametrize(
    "spec,code",
    [
        ({"from": "stock", "let": 5, "pipeline": [], "as": "s"}, 14),
        ({"from": "stock", "pipeline": 5, "as": "s"}, 14),
    ],
)
def test_wrong_typed_let_and_pipeline_do_not_crash(db, spec, code) -> None:
    """``let: 5`` reached ``.items()`` and ``pipeline: 5`` was iterated -- both
    raised bare exceptions that escaped as "internal server error"."""
    _seed(db, ORDERS, STOCK)
    err = _err(db, [{"$lookup": spec}])
    assert err.code == code
    assert "internal server error" not in str(err)


# --- $lookup argument errors -------------------------------------------------


@pytest.mark.parametrize(
    "spec,fragment",
    [
        # `from` keeps its hand-written errors on 8.x; everything else here
        # moved to the IDL wording, which names the field as `$lookup.<name>`.
        ({"localField": "sku", "foreignField": "sku", "as": "s"}, "must specify 'pipeline'"),
        (
            {"from": "stock", "localField": "sku", "foreignField": "sku"},
            "BSON field '$lookup.as' is missing but a required field",
        ),
        (
            {"from": "stock", "localField": "sku", "as": "s"},
            "both or neither of 'localField' and 'foreignField'",
        ),
        ({"from": 5, "localField": "sku", "foreignField": "sku", "as": "s"}, "must be a string"),
        (
            {"from": "stock", "localField": 5, "foreignField": "sku", "as": "s"},
            "BSON field '$lookup.localField' is the wrong type",
        ),
        (
            {"from": "stock", "localField": "sku", "foreignField": "sku", "as": 5},
            "BSON field '$lookup.as' is the wrong type",
        ),
    ],
)
def test_lookup_argument_errors_name_the_field(db, spec, fragment) -> None:
    """We answered TypeMismatch (14) with one of two generic sentences that
    named neither the field nor the problem. mongod 8.x parses $lookup through
    the IDL, so most of these name the field as ``$lookup.<name>``; ``from`` is
    the exception and keeps its hand-written wording."""
    _seed(db, ORDERS, STOCK)
    err = _err(db, [{"$lookup": spec}])
    assert err.code in (9, 14, 40414)
    assert fragment in str(err)


def test_unknown_lookup_argument_is_rejected(db) -> None:
    """Accepted and ignored before, so a misspelled `foreignFeild` silently
    became a join over the whole foreign collection."""
    _seed(db, ORDERS, STOCK)
    err = _err(db, [{"$lookup": {**JOIN, "zz": 1}}])
    assert err.code == 40415
    assert "BSON field '$lookup.zz' is an unknown field." in str(err)


# --- $lookup join semantics --------------------------------------------------


def test_an_empty_array_local_field_matches_null(db) -> None:
    """mongod unwinds the local array for matching, and an empty one still
    joins against the null-valued foreign rows. We produced no lookup keys at
    all, so it matched nothing. BOTH join paths had it -- the hash join and the
    index-driven one -- and the index path's comment asserted mongod semantics
    the oracle contradicts."""
    _seed(db, [{"_id": 1, "tags": []}], STOCK)
    got = next(
        iter(
            db.orders.aggregate(
                [
                    {
                        "$lookup": {
                            "from": "stock",
                            "localField": "tags",
                            "foreignField": "sku",
                            "as": "s",
                        }
                    }
                ]
            )
        )
    )
    assert [d["_id"] for d in got["s"]] == [11]


def test_an_empty_array_matches_null_through_the_index_path(db) -> None:
    """The same case with an index on the foreign field, which routes through
    ``_index_join_lookup`` instead of the hash join."""
    _seed(db, [{"_id": 1, "tags": []}], STOCK)
    db.stock.create_index([("sku", 1)])
    got = next(
        iter(
            db.orders.aggregate(
                [
                    {
                        "$lookup": {
                            "from": "stock",
                            "localField": "tags",
                            "foreignField": "sku",
                            "as": "s",
                        }
                    }
                ]
            )
        )
    )
    assert [d["_id"] for d in got["s"]] == [11]


def test_a_dotted_as_builds_the_nesting(db) -> None:
    """``as`` is a PATH, not a key: ``as: "a.b"`` produces ``{a: {b: [...]}}``.
    We stored a literal key with a dot in it -- the same bug, and the same fix,
    as the dotted-equality upsert seed."""
    _seed(db, ORDERS, STOCK)
    got = next(iter(db.orders.aggregate([{"$lookup": {**JOIN, "as": "a.b"}}, {"$limit": 1}])))
    assert got["a"]["b"][0]["_id"] == 10
    assert "a.b" not in got


def test_a_dotted_as_on_graphlookup_too(db) -> None:
    _seed(db, [{"_id": 1, "sku": "a"}], CHAIN)
    got = next(iter(db.orders.aggregate([{"$graphLookup": {**GRAPH, "as": "x.y"}}])))
    assert isinstance(got["x"]["y"], list)
    assert "x.y" not in got


def test_a_plain_join_is_unchanged(db) -> None:
    _seed(db, ORDERS, STOCK)
    got = sorted(db.orders.aggregate([{"$lookup": JOIN}]), key=lambda d: d["_id"])
    assert [d["_id"] for d in got[0]["s"]] == [10]
    assert got[1]["s"] == []
