"""A non-document element in the ``pipeline`` array must be rejected with
mongod's exact error, not crash.

Probed against real mongod 8.0 — both the plain-``aggregate`` path and the
``changeStream`` path answer::

    code=14 (TypeMismatch)
    "Each element of the 'pipeline' array must be an object"

Before the fix, ``_apply_stage`` called ``len(stage)`` on the raw element, so
``pipeline: [42]`` raised ``TypeError: object of type 'int' has no len()`` and
the client saw a bare ``internal server error`` (code 1). libmongoc's
``/change_stream/accepts_array`` asserts on the message verbatim.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


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


MSG = "Each element of the 'pipeline' array must be an object"

# ``db.aggregate``/``db.watch`` validate the pipeline client-side before the
# wire (a scalar stage trips pymongo's own ``"$out" in pipeline[-1]``), so
# these drive ``db.command`` -- which is what libmongoc does, and the only way
# to prove the *server* rejects it.


def _agg(db, pipeline):
    return db.command({"aggregate": "c", "pipeline": pipeline, "cursor": {}})


@pytest.mark.parametrize("bad", [42, "stage", ["nested"], None, 3.5, True])
def test_aggregate_rejects_non_document_stage(client, bad) -> None:
    db = client["pipeline_elem"]
    db.c.insert_one({"x": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _agg(db, [bad])
    assert exc.value.code == 14, f"{bad!r} -> code {exc.value.code}"
    assert MSG in str(exc.value), f"{bad!r} -> {exc.value!s}"


@pytest.mark.parametrize("bad", [42, "stage", ["nested"]])
def test_change_stream_rejects_non_document_stage(client, bad) -> None:
    db = client["pipeline_elem_cs"]
    db.c.insert_one({"x": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _agg(db, [{"$changeStream": {}}, bad])
    assert exc.value.code == 14, f"{bad!r} -> code {exc.value.code}"
    assert MSG in str(exc.value), f"{bad!r} -> {exc.value!s}"


def test_valid_pipeline_still_runs(client) -> None:
    """The type check must not reject well-formed stages."""
    db = client["pipeline_elem_ok"]
    db.c.insert_many([{"x": 1}, {"x": 2}, {"x": 3}])
    got = list(db.c.aggregate([{"$match": {"x": {"$gt": 1}}}, {"$count": "n"}]))
    assert got == [{"n": 2}]


ARITY_MSG = "A pipeline stage specification object must contain exactly one field."


@pytest.mark.parametrize(
    "stage",
    [
        {"$match": {}, "$count": "n"},
        {"$limit": 1, "$count": "n"},
        {},
    ],
)
def test_wrong_arity_stage_uses_mongod_code_and_wording(client, stage) -> None:
    """A stage that *is* a document but isn't a single ``{op: spec}`` pair gets
    mongod's Location40323 -- a different code and message from the wrong-type
    error above. We previously answered 14 with our own wording for both.

    The leading ``{"$match": ...}`` case is the one that mattered most: the
    initial-filter lift matched on ``"$match" in stage`` alone, so a two-key
    stage had its filter hoisted and the stage dropped -- no error at all, and
    the ``$count`` silently discarded.
    """
    db = client["pipeline_elem_arity"]
    db.c.insert_one({"x": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _agg(db, [stage])
    assert exc.value.code == 40323, f"{stage!r} -> code {exc.value.code}"
    assert ARITY_MSG in str(exc.value), f"{stage!r} -> {exc.value!s}"
    assert MSG not in str(exc.value)
