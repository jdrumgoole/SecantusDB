"""`$rename`'s two refusals, as measured on mongod 8.2.11 (2026-09-09).

mongod separates them by WHEN it can decide:

* a **dynamic** component (`$`, `$[]`, `$[id]`) in either path is a PARSE error
  -- raised without looking at the document, so an absent source still errors,
  and reported bare;
* a path that indexes into an **array** is an EXECUTION error -- discovered per
  document, reported under `Plan executor error during update :: caused by ::`,
  and skipped entirely when the source field is absent, because then the
  `$rename` is a no-op.

Precedence, measured: source-dynamic > destination-dynamic > source-array >
destination-array.
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Decimal128, ObjectId

from secantus import SecantusDBServer

EXEC = "Plan executor error during update :: caused by :: "


@pytest.fixture
def coll(tmp_path):
    server = SecantusDBServer(port=0, storage_path=str(tmp_path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["ren"]["c"]
    finally:
        client.close()
        server.stop()


SEED = {"_id": 1, "v": [{"a": 1}, {"a": 2}], "w": {"a": 1}, "z": 5, "deep": {"n": [{"a": 1}]}}


def _fails(coll, update, seed=None, **kwargs):
    coll.delete_many({})
    coll.insert_one(dict(seed or SEED))
    with pytest.raises(pymongo.errors.WriteError) as excinfo:
        coll.update_one({"_id": 1}, update, **kwargs)
    return excinfo.value


# --- dynamic paths: parse time, bare, document-independent -----------------

DYNAMIC = [
    ("source-all", {"$rename": {"v.$[].a": "q"}}, "source", "v.$[].a"),
    ("source-dollar", {"$rename": {"v.$.a": "q"}}, "source", "v.$.a"),
    ("dest-all", {"$rename": {"z": "v.$[].b"}}, "destination", "v.$[].b"),
    ("dest-dollar", {"$rename": {"z": "v.$.b"}}, "destination", "v.$.b"),
    # The source's dynamic component outranks the destination's.
    ("both", {"$rename": {"v.$[].a": "v.$[].b"}}, "source", "v.$[].a"),
    # ...and a dynamic DESTINATION outranks an array-element SOURCE.
    (
        "dest-dynamic-beats-source-array",
        {"$rename": {"v.0.a": "v.$[].b"}},
        "destination",
        "v.$[].b",
    ),
]


@pytest.mark.parametrize("name,update,side,path", DYNAMIC, ids=[c[0] for c in DYNAMIC])
def test_dynamic_rename_path_is_refused(coll, name, update, side, path):
    err = _fails(coll, update)
    assert err.code == 2
    assert err.details["errmsg"] == f"The {side} field for $rename may not be dynamic: {path}"


def test_dynamic_is_decided_without_the_document(coll):
    """An absent source field still errors -- mongod decides this at parse time."""
    err = _fails(coll, {"$rename": {"nope.$[].x": "q"}})
    assert err.details["errmsg"] == "The source field for $rename may not be dynamic: nope.$[].x"


def test_dynamic_refusal_beats_array_filters_being_supplied(coll):
    """`$[e]` is refused even when its array filter exists."""
    err = _fails(coll, {"$rename": {"v.$[e].a": "q"}}, array_filters=[{"e.a": 1}])
    assert err.details["errmsg"] == "The source field for $rename may not be dynamic: v.$[e].a"


def test_missing_array_filter_is_reported_first(coll):
    """Without the filter, the general arrayFilters check wins."""
    err = _fails(coll, {"$rename": {"v.$[e].a": "q"}})
    assert err.details["errmsg"] == "No array filter found for identifier 'e' in path 'v.$[e].a'"


# --- array elements: execution time, wrapped, source must resolve ----------


@pytest.mark.parametrize(
    "update,side,path,field",
    [
        ({"$rename": {"v.0.a": "v.0.b"}}, "source", "v.0.a", "v"),
        ({"$rename": {"v.0.a": "q"}}, "source", "v.0.a", "v"),
        # An index with nothing after it is still an array element.
        ({"$rename": {"v.0": "q"}}, "source", "v.0", "v"),
        # The message names the field HOLDING the array, not the whole path.
        ({"$rename": {"deep.n.0.a": "q"}}, "source", "deep.n.0.a", "n"),
        ({"$rename": {"z": "v.0.b"}}, "destination", "v.0.b", "v"),
        ({"$rename": {"z": "deep.n.0.a"}}, "destination", "deep.n.0.a", "n"),
    ],
)
def test_array_element_rename_is_refused(coll, update, side, path, field):
    err = _fails(coll, update)
    assert err.code == 2
    assert err.details["errmsg"] == (
        f"{EXEC}The {side} field cannot be an array element, '{path}' in doc "
        f"with _id: 1 has an array field called '{field}'"
    )


@pytest.mark.parametrize(
    "update",
    [
        # Index past the end: the source does not resolve, so it is a no-op.
        {"$rename": {"v.9.a": "q"}},
        # Leaf missing under the array: likewise.
        {"$rename": {"v.0.zz": "q"}},
        # An absent plain source is a no-op, and its array DESTINATION is never
        # reached -- this is why the array checks are gated on the source.
        {"$rename": {"nope": "v.0.b"}},
        {"$rename": {"nope": "q"}},
        # A numeric component against a DOCUMENT is an ordinary field name.
        {"$rename": {"w.0": "w.1"}},
    ],
)
def test_rename_with_an_unresolvable_source_is_a_no_op(coll, update):
    coll.delete_many({})
    coll.insert_one(dict(SEED))
    coll.update_one({"_id": 1}, update)
    assert coll.find_one({"_id": 1}) == SEED


def test_renaming_the_array_itself_is_fine(coll):
    coll.insert_one(dict(SEED))
    coll.update_one({"_id": 1}, {"$rename": {"v": "u"}})
    out = coll.find_one({"_id": 1})
    assert out["u"] == [{"a": 1}, {"a": 2}]
    assert "v" not in out


@pytest.mark.parametrize(
    "id_value,rendered",
    [
        (1, "1"),
        ("abc", '"abc"'),
        (ObjectId("507f1f77bcf86cd799439011"), "ObjectId('507f1f77bcf86cd799439011')"),
        (1.5, "1.5"),
        (Decimal128("2.5"), "2.5"),
        (None, "null"),
    ],
)
def test_the_id_in_the_message_uses_mongods_value_rendering(coll, id_value, rendered):
    """Not `str()`: a string `_id` is quoted and an ObjectId is wrapped."""
    coll.delete_many({})
    coll.insert_one({"_id": id_value, "v": [{"a": 1}]})
    with pytest.raises(pymongo.errors.WriteError) as excinfo:
        coll.update_one({"_id": id_value}, {"$rename": {"v.0.a": "q"}})
    assert (
        f"in doc with _id: {rendered} has an array field called 'v'"
        in (excinfo.value.details["errmsg"])
    )
