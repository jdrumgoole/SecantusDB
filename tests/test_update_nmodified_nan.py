"""``nModified``, and the phantom write behind it, on documents holding a NaN.

The write guard asked "did the document change" with a value comparison, and
``NaN != NaN``. So an update that touched *nothing* — an ``$unset`` of a missing
field, a ``$rename`` of a missing source, a ``$pull`` that matched nothing —
looked like a change on any document containing a NaN **anywhere**, including
nested in a subdocument or inside an array. The document was rewritten, an oplog
entry emitted, a change-stream event fired, and ``nModified`` came back 1. mongod
reports 0 and writes nothing (probed 8.2.11, 2026-09-06).

On the Python server this was hidden by an accident of container equality —
``==`` short-circuits on object identity, so an untouched value compared equal.
The Rust server has no such accident and carried the bug in full. Relying on the
accident was never safe, so both servers now use the encoded BSON.

The encoding alone is not all of mongod's rule, though, and the remainder is the
interesting part::

    {$inc: {a: 1}}   over a: NaN   -> nModified 1   (wrote a fresh NaN)
    {$inc: {a: 0}}   over a: NaN   -> nModified 1   (same)
    {$min: {a: 5}}   over a: NaN   -> nModified 0   ($min declined; NaN sorts lowest)
    {$set: {a: NaN}} over a: NaN   -> nModified 0   ($set wrote an equal value)
    {$inc: {a: 0}}   over a: 1     -> nModified 0   (wrote, but nothing changed)

Those five have byte-identical before and after images in every case, so no
document comparison can separate them. The discriminator is exactly "an
arithmetic operator produced a NaN" — ``update.arith_wrote_nan``.

Two earlier attempts are recorded in ``tasks/backlog.md`` as measured failures:
a byte comparison alone (loses the ``$inc``-over-NaN cases) and a BSON value
comparison alone (loses them the other way, since ``Bson::Double(NaN)`` is
unequal to itself). Both are subsumed here.
"""

from __future__ import annotations

import math

import pymongo
import pytest

from secantus import SecantusDBServer
from secantus.storage import _doc_changed
from secantus.update import arith_wrote_nan

NAN = float("nan")
INF = float("inf")


@pytest.fixture
def client(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


def _update(db, u, seed):
    coll = db["c"]
    coll.drop()
    coll.insert_one({"_id": 1, **seed})
    reply = db.command("update", "c", updates=[{"q": {"_id": 1}, "u": u}])
    assert "writeErrors" not in reply, reply
    return reply, coll


# --- the predicates ---------------------------------------------------------


def test_doc_changed_ignores_an_untouched_nan():
    """Two NaNs with the same bits encode the same, so nothing changed."""
    assert not _doc_changed({"a": NAN, "b": 1}, {"a": NAN, "b": 1})
    assert not _doc_changed({"a": {"n": NAN}}, {"a": {"n": NAN}})
    assert not _doc_changed({"a": [NAN]}, {"a": [NAN]})


def test_doc_changed_still_sees_a_signed_zero_and_a_type_change():
    """The 2026-09-05 fix must survive: these are the reason bytes are used."""
    assert _doc_changed({"a": -0.0}, {"a": 0.0})
    assert _doc_changed({"a": 0.0}, {"a": -0.0})
    assert _doc_changed({"a": 1.0}, {"a": 1})
    assert _doc_changed({"a": {"b": -0.0}}, {"a": {"b": 0.0}})
    assert _doc_changed({"a": [-0.0]}, {"a": [0.0]})


def test_doc_changed_sees_ordinary_differences():
    assert _doc_changed({"a": "x"}, {"a": "y"})
    assert _doc_changed({"a": 1}, {"a": 2})
    assert _doc_changed({"a": 1}, {"b": 1})
    assert _doc_changed({"a": 1}, {"a": 1, "b": 2})
    assert not _doc_changed({"a": 1, "b": "x"}, {"a": 1, "b": "x"})


def test_arith_wrote_nan_only_fires_for_an_arithmetic_nan():
    assert arith_wrote_nan({"a": NAN}, {"$inc": {"a": 1}})
    assert arith_wrote_nan({"a": NAN}, {"$mul": {"a": 2}})
    # Not arithmetic.
    assert not arith_wrote_nan({"a": NAN}, {"$set": {"a": NAN}})
    assert not arith_wrote_nan({"a": NAN}, {"$min": {"a": 5}})
    # Arithmetic, but the result is not a NaN.
    assert not arith_wrote_nan({"a": 1}, {"$inc": {"a": 0}})
    # A different field's NaN is not this operator's doing.
    assert not arith_wrote_nan({"a": 1, "b": NAN}, {"$inc": {"a": 0}})


def test_arith_wrote_nan_accepts_a_pipeline_update():
    """A pipeline update is a LIST, and the guard calls this on every update.

    Reaching ``.get`` on a list raised, and the command layer surfaced it as an
    InternalError -- ``findAndModify`` with ``update: []`` stopped working
    entirely. Caught by the mongod differential gate, not by any unit test,
    which is why one lives here now.
    """
    assert not arith_wrote_nan({"a": NAN}, [])
    assert not arith_wrote_nan({"a": NAN}, [{"$set": {"b": 1}}])


def test_an_empty_pipeline_update_is_a_no_op(client):
    db = client["nmod"]
    coll = db["c"]
    coll.drop()
    coll.insert_one({"_id": 1, "n": 5, "a": NAN})
    db.command({"findAndModify": "c", "query": {"_id": 1}, "update": []})
    got = coll.find_one({"_id": 1})
    assert got["n"] == 5
    assert math.isnan(got["a"])


# --- over the wire ----------------------------------------------------------

# An update that touches NOTHING must not report a modification, whatever else
# the document happens to contain.
_NO_OP_ON_A_NAN_DOCUMENT = [
    ({"a": NAN, "b": 1}, {"$unset": {"absent": ""}}),
    ({"a": NAN, "b": 1}, {"$set": {"b": 1}}),
    ({"a": NAN, "b": 1}, {"$inc": {"b": 0}}),
    ({"a": NAN, "b": [1]}, {"$pull": {"b": 9}}),
    ({"a": NAN, "b": [1]}, {"$addToSet": {"b": 1}}),
    ({"a": NAN, "b": 1}, {"$rename": {"absent": "gone"}}),
    ({"a": {"n": NAN}, "b": 1}, {"$set": {"b": 1}}),
    ({"a": [NAN], "b": 1}, {"$set": {"b": 1}}),
]


@pytest.mark.parametrize(("seed", "update"), _NO_OP_ON_A_NAN_DOCUMENT)
def test_a_no_op_on_a_nan_document_reports_nothing_modified(client, seed, update):
    reply, _ = _update(client["nmod"], update, seed)
    assert (reply["n"], reply["nModified"]) == (1, 0)


@pytest.mark.parametrize(("seed", "update"), _NO_OP_ON_A_NAN_DOCUMENT)
def test_a_no_op_on_a_nan_document_emits_no_change_event(client, seed, update):
    """The counter was the symptom; the phantom WRITE was the defect."""
    db = client["nmod"]
    coll = db["c"]
    coll.drop()
    coll.insert_one({"_id": 1, **seed})
    with coll.watch() as stream:
        db.command("update", "c", updates=[{"q": {"_id": 1}, "u": update}])
        assert stream.try_next() is None, "a no-op update must not emit an event"


# mongod's per-operator rule, over five byte-identical before/after images.
@pytest.mark.parametrize(
    ("seed", "update", "n_modified"),
    [
        ({"a": NAN}, {"$inc": {"a": 1}}, 1),
        ({"a": NAN}, {"$inc": {"a": 0}}, 1),
        ({"a": NAN}, {"$mul": {"a": 2}}, 1),
        ({"a": NAN}, {"$set": {"a": NAN}}, 0),
        ({"a": NAN}, {"$min": {"a": 5}}, 0),
        ({"a": NAN}, {"$min": {"a": -INF}}, 0),
        ({"a": 1}, {"$inc": {"a": 0}}, 0),
    ],
)
def test_the_arithmetic_nan_rule(client, seed, update, n_modified):
    reply, coll = _update(client["nmod"], update, seed)
    assert reply["nModified"] == n_modified
    # Whatever the count, the stored value is unchanged in every one of these.
    got = coll.find_one({"_id": 1})["a"]
    if isinstance(seed["a"], float) and math.isnan(seed["a"]):
        assert math.isnan(got)
    else:
        assert got == seed["a"]


# An operator that DOES change the value still counts, NaN or not.
@pytest.mark.parametrize(
    ("seed", "update", "expected"),
    [
        ({"a": NAN}, {"$max": {"a": 5}}, 5),
        ({"a": NAN}, {"$max": {"a": -INF}}, -INF),
        ({"a": 5}, {"$min": {"a": NAN}}, NAN),
    ],
)
def test_an_operator_that_changes_the_value_still_counts(client, seed, update, expected):
    reply, coll = _update(client["nmod"], update, seed)
    assert reply["nModified"] == 1
    got = coll.find_one({"_id": 1})["a"]
    if isinstance(expected, float) and math.isnan(expected):
        assert math.isnan(got)
    else:
        assert got == expected
