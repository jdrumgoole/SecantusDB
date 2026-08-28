"""``findAndModify`` argument validation and reply shape match mongod's.

Found by differential-probing 18 findAndModify shapes against a real mongod:
6 diverged. One was a crash, two were silent acceptances of commands mongod
rejects, and one returned an update-shaped reply for a delete.

Probed on mongod **6.0.16** — the version the live differential gate
(``tests/test_mongod_differential.py``) spawns — and cross-checked on 8.3.4.
All the behaviour below is identical on both; only the error *wording* differs
(8.3 quotes the field names, e.g. ``both an 'update' and 'remove'=true``).
SecantusDB advertises 7.0, so 6.0's wording is what ships.
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
        yield cli["fam"]
    finally:
        cli.close()
        srv.stop()


def _fam(db, **extras):
    cmd = {"findAndModify": "c"}
    cmd.update(extras)
    return db.command(cmd)


def test_bad_update_type_does_not_crash(db) -> None:
    """The regression: a non-document `update` reached apply_update, which did
    `update.keys()` and raised AttributeError -- surfacing as a bare
    "internal server error" (code 1)."""
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update=5)
    assert exc.value.code == 9
    assert "Update argument must be either an object or an array" in str(exc.value)
    assert "internal server error" not in str(exc.value)


@pytest.mark.parametrize("bad", [5, "x", 1.5, True])
def test_every_non_document_update_is_rejected(db, bad) -> None:
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, update=bad)
    assert exc.value.code == 9


def test_remove_with_new_is_rejected(db) -> None:
    """mongod refuses rather than ignoring `new` -- a remove has no "after"
    document. We used to accept it and delete anyway."""
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, remove=True, new=True)
    assert exc.value.code == 9
    assert "new=true and remove=true" in str(exc.value)
    assert db.c.count_documents({}) == 1, "the document must not be removed"


def test_remove_with_upsert_is_rejected(db) -> None:
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, remove=True, upsert=True)
    assert exc.value.code == 9
    assert "upsert=true and remove=true" in str(exc.value)
    assert db.c.count_documents({}) == 1


def test_remove_with_update_is_rejected(db) -> None:
    db.c.insert_one({"_id": 1})
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _fam(db, query={"_id": 1}, remove=True, update={"$set": {"a": 1}})
    assert exc.value.code == 9
    assert "an update and remove=true" in str(exc.value)


def test_remove_reply_omits_updated_existing(db) -> None:
    """`updatedExisting` describes an UPDATE; mongod omits it for a remove.
    We emitted it, so a driver reading the field saw an update-shaped reply
    for a delete."""
    db.c.insert_one({"_id": 1, "a": 1})
    reply = _fam(db, query={"_id": 1}, remove=True)
    assert reply["lastErrorObject"] == {"n": 1}
    assert reply["value"] == {"_id": 1, "a": 1}


def test_remove_no_match_reply_omits_updated_existing(db) -> None:
    db.c.insert_one({"_id": 1})
    reply = _fam(db, query={"_id": 9}, remove=True)
    assert reply["lastErrorObject"] == {"n": 0}
    assert reply["value"] is None


def test_update_replies_still_carry_updated_existing(db) -> None:
    """The omission is remove-only -- an update must keep the field."""
    db.c.insert_one({"_id": 1, "a": 1})
    reply = _fam(db, query={"_id": 1}, update={"$set": {"a": 2}})
    assert reply["lastErrorObject"] == {"n": 1, "updatedExisting": True}

    miss = _fam(db, query={"_id": 9}, update={"$set": {"a": 2}})
    assert miss["lastErrorObject"] == {"n": 0, "updatedExisting": False}


def test_upsert_reply_shape_is_unchanged(db) -> None:
    reply = _fam(db, query={"_id": 5}, update={"$set": {"a": 1}}, upsert=True)
    leo = reply["lastErrorObject"]
    assert leo["n"] == 1
    assert leo["updatedExisting"] is False
    assert leo["upserted"] == 5


def test_valid_remove_and_update_still_work(db) -> None:
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])
    assert _fam(db, query={"_id": 1}, update={"$set": {"a": 9}}, new=True)["value"]["a"] == 9
    assert _fam(db, query={"_id": 2}, remove=True)["value"] == {"_id": 2, "a": 2}
    assert db.c.count_documents({}) == 1
