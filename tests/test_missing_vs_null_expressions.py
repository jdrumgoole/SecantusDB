"""A missing field is not null — in the *expression* language.

A targeted sweep, motivated by the Phase 2 campaign: ``get_path`` returns
``None`` for both an absent field and an explicit null, so anywhere the code
tested a resolved value for ``is None`` it had conflated them. That confusion
produced both ``$graphLookup`` bugs and the ``$lookup`` ``let`` gap.

25 shapes were probed against mongod 6.0.16. **22 were already right** — the
*query* language deliberately does treat them alike (``{a: null}`` matches a
missing field, and that is mongod's behaviour too). The divergence was confined
to the *comparison operators*, where mongod draws the line:

* ``$eq: ["$absent", null]`` is **false**; ``$eq: ["$explicitNull", null]`` is true
* a missing field ranks **below every real value**, MinKey included
  (``$cmp: ["$absent", MinKey]`` is -1)
* a missing field equals only another missing field

Probed against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

DOCS = [{"_id": 1, "a": None}, {"_id": 2}, {"_id": 3, "a": 5}]


# The server is module-scoped, but `db` stays per-test: this file's fixture
# SEEDS data, and seeding once while a shared server kept the rows would let
# one test's writes reach the next. Each test gets a fresh client, a fresh
# seed, and drops its database on the way out -- so only the ~236 ms store
# open is shared, not any state.
@pytest.fixture(scope="module")
def _server(wt_home_module):
    with SecantusDBServer(port=0, storage_path=wt_home_module) as srv:
        yield srv


@pytest.fixture
def db(_server):
    cli = pymongo.MongoClient(
        f"mongodb://{_server.address[0]}:{_server.address[1]}", directConnection=True
    )
    try:
        d = cli["mn"]
        d.c.insert_many([dict(x) for x in DOCS])
        yield d
    finally:
        cli.drop_database(d.name)
        cli.close()


def _proj(db, expr):
    return {
        d["_id"]: d.get("r")
        for d in db.c.aggregate([{"$project": {"r": expr}}, {"$sort": {"_id": 1}}])
    }


def test_eq_null_is_false_for_a_missing_field(db) -> None:
    """The regression: every comparison against null answered true for
    documents that did not have the field at all."""
    assert _proj(db, {"$eq": ["$a", None]}) == {1: True, 2: False, 3: False}


def test_eq_is_symmetric(db) -> None:
    assert _proj(db, {"$eq": [None, "$a"]}) == {1: True, 2: False, 3: False}


def test_ne_null_mirrors_eq(db) -> None:
    assert _proj(db, {"$ne": ["$a", None]}) == {1: False, 2: True, 3: True}


def test_missing_equals_only_missing(db) -> None:
    assert _proj(db, {"$eq": ["$a", "$nope"]}) == {1: False, 2: True, 3: False}


def test_missing_ranks_below_null(db) -> None:
    assert _proj(db, {"$cmp": ["$a", None]}) == {1: 0, 2: -1, 3: 1}


def test_missing_ranks_below_every_value(db) -> None:
    """Below MinKey too -- probed."""
    assert _proj(db, {"$lt": ["$a", False]})[2] is True
    assert _proj(db, {"$lt": ["$a", 0]})[2] is True
    assert _proj(db, {"$lt": ["$a", "x"]})[2] is True
    assert _proj(db, {"$gt": ["$a", False]})[2] is False


def test_missing_compares_equal_to_missing(db) -> None:
    assert _proj(db, {"$cmp": ["$a", "$nope"]})[2] == 0
    assert _proj(db, {"$lte": ["$a", "$nope"]})[2] is True
    assert _proj(db, {"$gte": ["$a", "$nope"]})[2] is True
    assert _proj(db, {"$lt": ["$a", "$nope"]})[2] is False


def test_cond_built_on_eq_inherits_the_fix(db) -> None:
    """`$cond` had no bug of its own -- it was wrong because `$eq` was."""
    assert _proj(db, {"$cond": [{"$eq": ["$a", None]}, "y", "n"]}) == {1: "y", 2: "n", 3: "n"}


def test_let_binds_a_missing_field_as_missing(db) -> None:
    """A `$let` var bound from an absent field stayed missing rather than
    collapsing to null. The same rule governs `$lookup`'s `let`, where binding
    null made a document without the local field join rows mongod excludes."""
    expr = {"$let": {"vars": {"v": "$a"}, "in": {"$eq": ["$$v", None]}}}
    assert _proj(db, expr) == {1: True, 2: False, 3: False}


# --- the rules that were ALREADY right, pinned so the fix can't over-reach ---


def test_the_query_language_still_treats_them_alike(db) -> None:
    """`{a: null}` matches a missing field. This is mongod's behaviour and the
    OPPOSITE of the expression language -- the distinction is confined to
    comparison operators."""
    assert sorted(d["_id"] for d in db.c.find({"a": None})) == [1, 2]
    assert sorted(d["_id"] for d in db.c.find({"a": {"$exists": False}})) == [2]
    assert sorted(d["_id"] for d in db.c.find({"a": {"$type": "null"}})) == [1]


def test_a_missing_operator_argument_is_still_null(db) -> None:
    """Outside comparisons, a missing path is null -- and arithmetic over null
    is NULL, not a no-op operand.

    The docstring on ``_eval_field_value`` claimed ``{$add: ["$nope", 1]}`` is
    ``1``; mongod answers ``null`` (probed 6.0.16, same for an explicit null and
    for ``$multiply``). The comment was corrected alongside this test -- the
    behaviour was already right, only the note about it was wrong.
    """
    assert _proj(db, {"$add": ["$a", 1]}) == {1: None, 2: None, 3: 6}
    assert _proj(db, {"$multiply": ["$a", 2]}) == {1: None, 2: None, 3: 10}


def test_ifnull_still_treats_them_alike(db) -> None:
    assert _proj(db, {"$ifNull": ["$a", "F"]}) == {1: "F", 2: "F", 3: 5}


def test_type_still_distinguishes_them(db) -> None:
    assert _proj(db, {"$type": "$a"}) == {1: "null", 2: "missing", 3: "int"}


def test_project_still_omits_a_missing_path(db) -> None:
    out = {d["_id"]: d for d in db.c.aggregate([{"$project": {"r": "$a"}}])}
    assert "r" not in out[2]
    assert out[1]["r"] is None
