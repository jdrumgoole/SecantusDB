"""A retryable write's retry must not replay the original attempt's error.

A ``writeConcernError`` describes THIS attempt: the write itself succeeded and
only its durability acknowledgement failed. Replaying it on the retry hands the
driver the very error it retried because of, so the retry "fails" too and the
operation surfaces as an error even though the write is safely applied.

This kept libmongoc's ``/command_monitoring/unified/writeConcernError`` red. The
long-standing theory in the backlog was that the driver never classified the
write as retryable. Tracing the commands showed the opposite: it assigns a
``txnNumber`` and it does retry on a fresh connection -- and got the replayed
error back.

Both expectations below were probed against a real mongod 8.3.4 running as a
single-node replica set with ``enableTestCommands=1``.
"""

from __future__ import annotations

import uuid

import bson
import pymongo
import pytest
from bson.binary import UUID_SUBTYPE, Binary

from secantus import SecantusDBServer


@pytest.fixture
def client(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"), replica_set_name="secantus")
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


def _arm(cli, wce, labels=None):
    data = {"failCommands": ["insert"], "writeConcernError": wce}
    if labels:
        data["errorLabels"] = labels
    cli.admin.command({"configureFailPoint": "failCommand", "mode": {"times": 1}, "data": data})


def _lsid():
    return {"id": Binary(uuid.uuid4().bytes, UUID_SUBTYPE)}


def test_retry_does_not_replay_the_write_concern_error(client) -> None:
    """The regression. mongod: attempt 1 carries the wce, the retry does not."""
    db = client["rw"]
    _arm(client, {"code": 91}, ["RetryableWriteError"])
    cmd = {
        "insert": "c",
        "documents": [{"_id": 1, "x": 1}],
        "lsid": _lsid(),
        "txnNumber": bson.Int64(1),
    }
    first = db.command(dict(cmd))
    assert first["writeConcernError"] == {"code": 91}
    assert first["errorLabels"] == ["RetryableWriteError"]

    retry = db.command(dict(cmd))
    assert retry["n"] == 1
    assert "writeConcernError" not in retry, retry
    assert "errorLabels" not in retry, retry
    assert db.c.count_documents({}) == 1, "the retry must not double-apply"


def test_write_concern_error_is_echoed_verbatim(client) -> None:
    """mongod does not synthesise into the failpoint's document.

    We used to add ``errmsg`` and ``codeName``. The unified-spec matcher compares
    nested documents by exact key count, so that failed with "expected 1 keys in
    document, got: 3" -- and the synthesised ``codeName`` was wrong anyway,
    rendering 91 as ``Location91`` where 91 is ``ShutdownInProgress``.
    """
    db = client["rw_shape"]
    _arm(client, {"code": 91})
    reply = db.command({"insert": "c", "documents": [{"_id": 1}]})
    assert reply["writeConcernError"] == {"code": 91}


def test_supplied_errmsg_is_preserved(client) -> None:
    """Verbatim means verbatim -- a failpoint-supplied errmsg still comes back."""
    db = client["rw_msg"]
    _arm(client, {"code": 91, "errmsg": "custom"})
    reply = db.command({"insert": "c", "documents": [{"_id": 1}]})
    assert reply["writeConcernError"] == {"code": 91, "errmsg": "custom"}


def test_a_non_retryable_write_still_reports_the_error(client) -> None:
    """Stripping applies only to the REPLAY. A plain write with no lsid/txnNumber
    is not a retryable write and must still surface its writeConcernError."""
    db = client["rw_plain"]
    _arm(client, {"code": 91}, ["RetryableWriteError"])
    reply = db.command({"insert": "c", "documents": [{"_id": 1}]})
    assert reply["writeConcernError"] == {"code": 91}
    assert reply["errorLabels"] == ["RetryableWriteError"]


def test_retry_of_a_clean_write_is_still_idempotent(client) -> None:
    """The pre-existing replay behaviour is unchanged for the ordinary case."""
    db = client["rw_clean"]
    cmd = {
        "insert": "c",
        "documents": [{"_id": 7, "x": 1}],
        "lsid": _lsid(),
        "txnNumber": bson.Int64(1),
    }
    first = db.command(dict(cmd))
    retry = db.command(dict(cmd))
    assert first["n"] == retry["n"] == 1
    assert db.c.count_documents({}) == 1
