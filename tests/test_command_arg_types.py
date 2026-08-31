"""A wrong-typed command argument is a parse error, not a crash.

Twice in one session a caller-supplied scalar reached code that assumed a
document and crashed as ``internal server error`` (code 1): ``pipeline: [42]``
called ``len()`` on an int, ``update: 5`` called ``.keys()`` on one. Sweeping the
same shape across every document-valued command argument found the pattern was
systemic -- **45 of 56 probed argument slots crashed**.

mongod answers a parse error for all of them. The message family differs by
command, which is why this needs more than one blanket check:

* ``find`` -- ``Expected field filterto be of type object`` (mongod's own
  missing space; fidelity means reproducing it)
* ``count`` / ``distinct`` / ``delete`` / ``update`` / ``findAndModify`` --
  ``BSON field 'count.query' is the wrong type 'int', expected type 'object'``
* ``aggregate`` -- ``A pipeline must be an array of objects`` (re-probed
  8.2.11 2026-08-31; the old assertion here pinned OUR wording)
* ``update``'s ``u`` -- accepts an object OR an array, so a scalar is
  ``FailedToParse`` (9) while an array of non-documents is the pipeline-element
  error (14)

Probed on mongod 6.0.16 (the version the live differential gate spawns) and
cross-checked on 8.3.4: 56/56 identical on both.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

BAD = [5, "x", True, [1, 2]]


@pytest.fixture
def db(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    d = cli["argtypes"]
    d.c.insert_one({"_id": 1, "a": 1})
    try:
        yield d
    finally:
        cli.close()
        srv.stop()


def _err(db, cmd):
    """Return the OperationFailure raised by ``cmd``, failing if none is."""
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command(dict(cmd))
    assert exc.value.code != 1, f"crashed instead of parsing: {exc.value}"
    assert "internal server error" not in str(exc.value)
    return exc.value


@pytest.mark.parametrize("bad", BAD)
@pytest.mark.parametrize("field", ["filter", "sort", "projection"])
def test_find_document_args(db, field, bad) -> None:
    err = _err(db, {"find": "c", field: bad})
    assert err.code == 14
    assert err.details["errmsg"] == f"Expected field {field}to be of type object"


@pytest.mark.parametrize("bad", BAD)
@pytest.mark.parametrize(
    "cmd,path",
    [
        ({"count": "c"}, "count.query"),
        # 8.x names the IDL STRUCT for distinct, as it does for `.key`.
        ({"distinct": "c", "key": "a"}, "distinctCommandRequest.query"),
    ],
)
def test_query_arg_uses_the_bson_field_form(db, cmd, path, bad) -> None:
    full = dict(cmd)
    full["query"] = bad
    err = _err(db, full)
    assert err.code == 14
    assert err.details["errmsg"].startswith(f"BSON field '{path}' is the wrong type")


@pytest.mark.parametrize("bad", BAD)
def test_delete_statement_q(db, bad) -> None:
    err = _err(db, {"delete": "c", "deletes": [{"q": bad, "limit": 1}]})
    assert err.code == 14
    assert "delete.deletes.q" in err.details["errmsg"]


@pytest.mark.parametrize("bad", BAD)
def test_update_statement_q(db, bad) -> None:
    err = _err(db, {"update": "c", "updates": [{"q": bad, "u": {"$set": {"a": 1}}}]})
    assert err.code == 14
    assert "update.updates.q" in err.details["errmsg"]


@pytest.mark.parametrize("bad", [5, "x", True])
def test_update_statement_u_scalar_is_failed_to_parse(db, bad) -> None:
    """`u` accepts an object OR an array, so a scalar is 9, not a type mismatch."""
    err = _err(db, {"update": "c", "updates": [{"q": {}, "u": bad}]})
    assert err.code == 9
    assert err.details["errmsg"] == "Update argument must be either an object or an array"


def test_update_statement_u_array_of_non_documents(db) -> None:
    """An array `u` IS a pipeline, so its elements obey the pipeline rule."""
    err = _err(db, {"update": "c", "updates": [{"q": {}, "u": [1, 2]}]})
    assert err.code == 14
    assert err.details["errmsg"] == "Each element of the 'pipeline' array must be an object"


@pytest.mark.parametrize("bad", [5, "x", True])
def test_aggregate_pipeline_must_be_an_array(db, bad) -> None:
    err = _err(db, {"aggregate": "c", "pipeline": bad, "cursor": {}})
    assert err.code == 14
    assert err.details["errmsg"] == "A pipeline must be an array of objects"


@pytest.mark.parametrize("bad", BAD)
@pytest.mark.parametrize("field", ["query", "sort", "fields"])
def test_find_and_modify_document_args(db, field, bad) -> None:
    cmd = {"findAndModify": "c", "query": {}, "remove": True}
    cmd[field] = bad
    err = _err(db, cmd)
    assert err.code == 14
    assert f"findAndModify.{field}" in err.details["errmsg"]


def test_valid_arguments_are_unaffected(db) -> None:
    """The guards must not reject legitimate shapes -- including absent args."""
    assert list(db.c.find({}))
    assert db.command({"find": "c", "filter": {"a": 1}, "sort": {"a": 1}})["cursor"]["firstBatch"]
    assert db.command({"count": "c", "query": {"a": 1}})["n"] == 1
    assert db.command({"count": "c"})["n"] == 1  # query absent entirely
    assert db.command({"aggregate": "c", "pipeline": [], "cursor": {}})["ok"] == 1
    assert db.command({"update": "c", "updates": [{"q": {}, "u": [{"$set": {"b": 1}}]}]})["n"] == 1
