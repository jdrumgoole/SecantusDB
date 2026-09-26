"""The ``maxTimeAlwaysTimeOut`` failpoint, on both servers.

mongod's failpoint makes every operation that HAS a time limit expire at its
first interrupt check, however generous the budget, and leaves operations
without one alone. pymongo's own suite turns it on to provoke
``MaxTimeMSExpired`` deterministically (``test_max_time_ms``,
``test_max_time_ms_getmore``, ``test_index_management_max_time_ms``,
``test_command_max_time_ms``). Both servers accepted it and ignored it, so
those four tests failed with "ExecutionTimeout not raised".

A ``getMore`` is the subtle case: pymongo does not send ``maxTimeMS`` on a
non-tailable getMore, because mongod bounds the cursor by the ORIGINATING
command's limit. So the failpoint must fire on the getMore of a cursor opened
with ``maxTimeMS``, and must not fire on one opened without.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from pymongo import IndexModel, MongoClient
from pymongo.errors import ExecutionTimeout

from secantus import SecantusDBServer


@pytest.fixture(params=["python", "rust"])
def client(request, tmp_path) -> Iterator[MongoClient]:
    if request.param == "python":
        with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
            mc = MongoClient(srv.uri, serverSelectionTimeoutMS=3000)
            try:
                yield mc
            finally:
                mc.close()
        return
    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        mc = MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=3000)
        try:
            yield mc
        finally:
            mc.close()
    finally:
        srv.stop()


def _fp(client: MongoClient, mode) -> None:
    client.admin.command("configureFailPoint", "maxTimeAlwaysTimeOut", mode=mode)


def test_only_operations_with_a_time_limit_expire(client: MongoClient) -> None:
    coll = client["mt"]["c"]
    coll.insert_many([{"i": i} for i in range(5)])
    _fp(client, "alwaysOn")
    try:
        with pytest.raises(ExecutionTimeout) as info:
            coll.find_one(max_time_ms=60_000)
        assert info.value.code == 50
        assert info.value.details["codeName"] == "MaxTimeMSExpired"
        with pytest.raises(ExecutionTimeout):
            client["mt"].command("count", "c", maxTimeMS=60_000)
        with pytest.raises(ExecutionTimeout):
            list(coll.aggregate([{"$match": {}}], maxTimeMS=60_000))
        with pytest.raises(ExecutionTimeout):
            coll.create_indexes([IndexModel("i")], maxTimeMS=60_000)
        # No limit, no expiry.
        assert coll.count_documents({}) == 5
        assert len(list(coll.find())) == 5
    finally:
        _fp(client, "off")
    assert coll.find_one({"i": 1}, max_time_ms=60_000)["i"] == 1


def test_getmore_inherits_the_originating_limit(client: MongoClient) -> None:
    coll = client["mt"]["g"]
    coll.insert_many([{"i": i} for i in range(20)])
    limited = coll.find(batch_size=2).max_time_ms(60_000)
    unlimited = coll.find(batch_size=2)
    next(limited)
    next(unlimited)
    _fp(client, "alwaysOn")
    try:
        with pytest.raises(ExecutionTimeout):
            limited.to_list()
        assert len(unlimited.to_list()) == 19
    finally:
        _fp(client, "off")


def test_times_mode_fires_that_many_times(client: MongoClient) -> None:
    coll = client["mt"]["t"]
    coll.insert_one({"i": 1})
    _fp(client, {"times": 1})
    with pytest.raises(ExecutionTimeout):
        coll.find_one(max_time_ms=60_000)
    assert coll.find_one(max_time_ms=60_000)["i"] == 1
