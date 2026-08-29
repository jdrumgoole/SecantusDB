"""Two update-layer rules that were silently wrong on BOTH write commands.

Found by differential-probing ``findAndModify`` against mongod 6.0.16 and then
re-running the same shapes through the plain ``update`` command, which shared
the defects:

* ``update: {}`` is a **replacement with an empty document** -- mongod reduces
  the stored document to its ``_id``. We short-circuited on a falsy update and
  returned the document untouched, so every field the client asked to drop
  stayed, with ``ok: 1`` and no error. An empty *pipeline* (``[]``) is the
  genuine no-op, and stays one.
* A dotted path that runs *through* a non-document cannot be created. mongod
  answers ``PathNotViable`` (28); ``set_path`` returned silently, so the update
  reported success and changed nothing -- no data written, no error raised.

Plus the upsert-document shape (nesting from a dotted query, and field order),
which is the same code path.

Every expectation here was probed against mongod 6.0.16.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer
from secantus.paths import path_block
from secantus.update import UpdateError, apply_update


@pytest.fixture
def db(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli["upd"]
    finally:
        cli.close()
        srv.stop()


# --- the empty update document ---------------------------------------------


def test_empty_update_document_is_a_replacement() -> None:
    assert apply_update({"_id": 1, "n": 5, "s": "a"}, {}) == {"_id": 1}


def test_empty_update_document_without_an_id() -> None:
    """No ``_id`` to preserve -- an upsert seed, before storage assigns one."""
    assert apply_update({"n": 5}, {}) == {}


def test_empty_pipeline_is_a_no_op() -> None:
    """The distinction that makes the case above safe: ``[]`` is not ``{}``."""
    assert apply_update({"_id": 1, "n": 5}, []) == {"_id": 1, "n": 5}


def test_empty_update_over_the_wire_drops_the_fields(db) -> None:
    db.c.insert_one({"_id": 1, "n": 5, "s": "a"})
    reply = db.command({"update": "c", "updates": [{"q": {"_id": 1}, "u": {}}]})
    assert reply["n"] == 1
    assert reply["nModified"] == 1
    assert db.c.find_one({"_id": 1}) == {"_id": 1}


def test_empty_update_via_findandmodify(db) -> None:
    db.c.insert_one({"_id": 1, "n": 5, "s": "a"})
    reply = db.command({"findAndModify": "c", "query": {"_id": 1}, "update": {}})
    assert reply["lastErrorObject"] == {"n": 1, "updatedExisting": True}
    assert reply["value"] == {"_id": 1, "n": 5, "s": "a"}, "the pre-image is unabridged"
    assert db.c.find_one({"_id": 1}) == {"_id": 1}


def test_empty_update_matching_nothing_changes_nothing(db) -> None:
    db.c.insert_one({"_id": 1, "n": 5})
    reply = db.command({"update": "c", "updates": [{"q": {"_id": 9}, "u": {}}]})
    assert reply["n"] == 0
    assert db.c.find_one({"_id": 1}) == {"_id": 1, "n": 5}


# --- PathNotViable ----------------------------------------------------------


@pytest.mark.parametrize(
    "doc,path,expected",
    [
        # Blocked: a scalar stands where a document would have to be.
        ({"n": 5}, "n.x", ("n", 5, "x")),
        ({"n": None}, "n.x", ("n", None, "x")),
        ({"n": True}, "n.x", ("n", True, "x")),
        ({"a": {"b": 7}}, "a.b.c", ("b", 7, "c")),
        ({"a": {"b": {"c": 7}}}, "a.b.c.d", ("c", 7, "d")),
        # Blocked: an array addressed by a non-numeric component.
        ({"a": [1]}, "a.x", ("a", [1], "x")),
        # Blocked: descending into an array ELEMENT that is a scalar.
        ({"a": [1]}, "a.0.x", ("0", 1, "x")),
        # Creatable: mongod makes the missing sub-document.
        ({}, "n.x", None),
        ({"a": {}}, "a.b.c", None),
        # Creatable: an out-of-range index pads with nulls.
        ({"a": [1]}, "a.4", None),
        # Creatable: the leaf itself is simply overwritten, whatever its type.
        ({"n": 5}, "n", None),
        ({"a": {"b": 7}}, "a.b", None),
    ],
)
def test_path_block_names_what_is_in_the_way(doc, path, expected) -> None:
    assert path_block(doc, path) == expected


@pytest.mark.parametrize(
    "update",
    [
        {"$set": {"n.x": 1}},
        {"$inc": {"n.x": 1}},
        {"$push": {"n.x": 1}},
        {"$mul": {"n.x": 2}},
    ],
)
def test_creating_through_a_scalar_is_refused(update) -> None:
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "n": 5}, update)
    assert exc.value.code == 28
    assert str(exc.value) == "Cannot create field 'x' in element {n: 5}"
    assert exc.value.exec_error is True


def test_the_element_is_rendered_mongod_style() -> None:
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "a": [1]}, {"$set": {"a.x": 9}})
    assert str(exc.value) == "Cannot create field 'x' in element {a: [ 1 ]}"


@pytest.mark.parametrize(
    "doc,expected",
    [
        ({"_id": 1, "n": "s"}, "Cannot create field 'x' in element {n: \"s\"}"),
        ({"_id": 1, "n": None}, "Cannot create field 'x' in element {n: null}"),
        ({"_id": 1, "n": True}, "Cannot create field 'x' in element {n: true}"),
    ],
)
def test_scalar_rendering_matches_mongod(doc, expected) -> None:
    with pytest.raises(UpdateError) as exc:
        apply_update(doc, {"$set": {"n.x": 1}})
    assert str(exc.value) == expected


def test_unset_through_a_scalar_stays_a_no_op() -> None:
    """Only *creation* is refused -- ``$unset`` does not create, and mongod
    accepts it (probed)."""
    assert apply_update({"_id": 1, "n": 5}, {"$unset": {"n.x": ""}}) == {"_id": 1, "n": 5}


def test_out_of_range_index_still_pads_with_nulls() -> None:
    assert apply_update({"_id": 1, "a": [1]}, {"$set": {"a.4": 9}}) == {
        "_id": 1,
        "a": [1, None, None, None, 9],
    }


def test_missing_intermediate_is_still_created() -> None:
    assert apply_update({"_id": 1}, {"$set": {"n.x": 1}}) == {"_id": 1, "n": {"x": 1}}


def test_path_not_viable_over_the_wire(db) -> None:
    db.c.insert_one({"_id": 1, "n": 5})
    reply = db.command({"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"n.x": 1}}}]})
    assert reply["writeErrors"][0]["code"] == 28
    assert db.c.find_one({"_id": 1}) == {"_id": 1, "n": 5}


# --- the upserted document --------------------------------------------------


def test_upsert_from_a_dotted_query_nests(db) -> None:
    db.c.update_one({"sub.k": 77}, {"$set": {"y": 1}}, upsert=True)
    doc = db.c.find_one()
    assert doc["sub"] == {"k": 77}
    assert "sub.k" not in doc
    assert db.c.count_documents({"sub.k": 77}) == 1


def test_upserted_field_order(db) -> None:
    """``_id``, then the query-seeded fields, then the update's -- each group
    in field-name order (probed 6.0.16)."""
    db.c.update_one({"n": 1, "m": 2}, {"$set": {"z": 3, "a": 4}}, upsert=True)
    assert list(db.c.find_one()) == ["_id", "m", "n", "a", "z"]


def test_update_reply_field_order_matches_mongod(db) -> None:
    """``n``, then ``upserted`` / ``writeErrors``, then ``nModified``, ``ok``."""
    reply = db.command(
        {"update": "c", "updates": [{"q": {"s": "zz"}, "u": {"$set": {"y": 1}}, "upsert": True}]}
    )
    assert list(reply)[:4] == ["n", "upserted", "nModified", "ok"]

    errored = db.command({"update": "c", "updates": [{"q": {"s": "zz"}, "u": {"$nope": {"y": 1}}}]})
    assert list(errored)[:4] == ["n", "writeErrors", "nModified", "ok"]


# --- the shared "unknown modifier" wording ----------------------------------


@pytest.mark.parametrize(
    "update,named",
    [
        ({"$nope": {"n": 1}}, "$nope"),
        ({"$set": {"n": 1}, "z": 2}, "z"),
    ],
)
def test_unknown_modifier_message(update, named) -> None:
    """mongod has ONE complaint for both shapes, and for the mixed one it names
    the bare field with no ``$``. We had two different hand-written sentences,
    neither of which any real server emits."""
    with pytest.raises(UpdateError) as exc:
        apply_update({"_id": 1, "n": 5}, update)
    assert exc.value.code == 9
    assert str(exc.value) == (
        f"Unknown modifier: {named}. Expected a valid update modifier or "
        "pipeline-style update specified as an array"
    )
