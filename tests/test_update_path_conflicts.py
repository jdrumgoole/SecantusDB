"""An update whose operators touch overlapping paths must be rejected.

mongod refuses an update where two operators target paths that are equal, or
where one is a prefix of the other: ``{$set: {a: 2}, $inc: {"a.b": 1}}`` cannot
be applied because ``$set`` replaces the very subtree ``$inc`` wants to walk
into. Sibling and disjoint paths are fine.

We used to apply every operator regardless and return a document mongod would
have refused to produce -- ``{$set: {a: 2}, $inc: {a: 1}}`` yielded ``{a: 3}``.
Silently wrong, with no error to notice. Found by differential-probing the
update operator family against a real mongod rather than by any failing test.

Every expectation here was probed against mongod 8.3.4.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer
from secantus.update import UpdateError, apply_update


@pytest.fixture
def client(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


@pytest.mark.parametrize(
    "update,offending,at",
    [
        ({"$set": {"a": 2}, "$inc": {"a": 1}}, "a", "a"),
        ({"$set": {"a": 2}, "$unset": {"a": ""}}, "a", "a"),
        ({"$set": {"a": 2}, "$inc": {"a.b": 1}}, "a.b", "a"),
        ({"$set": {"a.b": 2}, "$inc": {"a": 1}}, "a", "a"),
        ({"$set": {"a.b": 2}, "$inc": {"a.b.c": 1}}, "a.b.c", "a.b"),
        ({"$set": {"a.0": 5}, "$inc": {"a.0": 1}}, "a.0", "a.0"),
        # $rename claims BOTH ends: it writes one and removes the other.
        ({"$rename": {"a": "b"}, "$set": {"b": 9}}, "b", "b"),
        ({"$rename": {"a": "c"}, "$set": {"a": 5}}, "a", "a"),
    ],
)
def test_conflicting_paths_are_rejected(update, offending, at) -> None:
    expected = f"Updating the path '{offending}' would create a conflict at '{at}'"
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": {"b": {"c": 1}}}, update)
    assert str(exc.value) == expected
    assert exc.value.code == 40


@pytest.mark.parametrize(
    "seed,update",
    [
        # Each case carries its own seed: `$inc` needs a numeric field, so the
        # sibling-path cases cannot share a doc with the dotted-path ones.
        ({"a": {"b": 1, "c": 1}}, {"$set": {"a.b": 2}, "$inc": {"a.c": 1}}),  # siblings
        ({"a": 1, "b": 1}, {"$set": {"a": 2}, "$inc": {"b": 1}}),  # disjoint
        ({"a": [1, 2]}, {"$set": {"a.0": 5}, "$inc": {"a.1": 1}}),  # sibling indexes
        ({"a": 1, "ab": 0}, {"$set": {"ab": 1}, "$inc": {"a": 1}}),  # NOT a path prefix
    ],
)
def test_non_overlapping_paths_are_allowed(seed, update) -> None:
    apply_update({"_id": 1, **seed}, update)


def test_prefix_check_is_component_wise(client) -> None:
    """'ab' is not a prefix of 'a' -- the check must split on dots, not compare
    strings, or every field sharing a leading substring would collide."""
    db = client["upc"]
    db.c.insert_one({"_id": 1, "a": 1, "ab": 2})
    db.command(
        {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"ab": 9}, "$inc": {"a": 1}}}]}
    )
    assert db.c.find_one({"_id": 1}) == {"_id": 1, "a": 2, "ab": 9}


def test_self_rename_keeps_its_own_error(client) -> None:
    """mongod has a dedicated error for `$rename: {a: a}` (code 2). The conflict
    check must not preempt it with a code-40 -- it did in the first cut."""
    db = client["upc_sr"]
    db.c.insert_one({"_id": 1, "a": 1})
    reply = db.command(
        {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$rename": {"a": "a"}}}]}
    )
    err = reply["writeErrors"][0]
    assert err["code"] == 2
    assert "must differ" in err["errmsg"]


@pytest.mark.parametrize(
    "update",
    [
        {"$rename": {"a": "a.b"}},  # destination inside the source
        {"$rename": {"a.b": "a"}},  # source inside the destination
    ],
)
def test_rename_endpoints_are_not_compared_with_each_other(update) -> None:
    """A `$rename` whose source and target overlap gets mongod's OWN error
    (code 2, "must not be on the same path"), not the code-40 conflict.

    The conflict scan therefore checks a field's paths against paths claimed by
    EARLIER operators, then claims them together -- comparing a rename's two
    endpoints with each other reported the wrong error, which the existing
    `test_rename_validation_and_no_corruption` caught.
    """
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": {"b": 1}}, update)
    assert exc.value.code == 2, str(exc.value)
    assert "conflict at" not in str(exc.value)


def test_conflict_surfaces_over_the_wire(client) -> None:
    db = client["upc_wire"]
    db.c.insert_one({"_id": 1, "a": 1})
    reply = db.command(
        {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 2}, "$inc": {"a": 1}}}]}
    )
    err = reply["writeErrors"][0]
    assert err["code"] == 40
    assert err["errmsg"] == "Updating the path 'a' would create a conflict at 'a'"
    assert db.c.find_one({"_id": 1}) == {"_id": 1, "a": 1}, "the doc must be untouched"
