"""RBAC coverage for aggregate secondary namespaces and configureFailPoint.

Issue #783: with ``--auth`` on, aggregate's RBAC check covered only the
primary collection — ``$out`` / ``$merge`` could write to (and drop) any
namespace and the ``$lookup`` family could read foreign namespaces with no
grant beyond ``find`` on the primary. Both servers now resolve a pipeline's
secondary-namespace requirements pre-execution (mongod's model): ``$out``
needs insert+remove on its target db, ``$merge`` insert+update, and the
read-side stages need find.

Issue #806: ``configureFailPoint`` was absent from both servers' RBAC action
tables, so any authenticated principal (even zero-role) could arm a
server-wide DoS failpoint. It now requires a cluster-admin grant.

Neither server implements a localhost exception, so each test seeds users
and roles on an auth-off server, restarts the same storage directory with
``require_auth`` on, and drives the checks over real pymongo connections.
"""

from __future__ import annotations

import contextlib

import pymongo
import pytest
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer

_READER_ROLE_PRIVS = [{"resource": {"db": "shop", "collection": "orders"}, "actions": ["find"]}]


def _seed(client: pymongo.MongoClient) -> None:
    """Users/roles created while auth is off: a find-only reader, a
    cluster-admin, and a zero-role principal."""
    admin = client.admin
    admin.command("createRole", "ordersReader", privileges=_READER_ROLE_PRIVS, roles=[])
    admin.command(
        "createUser",
        "reader",
        pwd="pw",
        roles=[{"role": "ordersReader", "db": "admin"}],
    )
    admin.command(
        "createUser",
        "opsadmin",
        pwd="pw",
        roles=[{"role": "clusterAdmin", "db": "admin"}],
    )
    admin.command("createUser", "norole", pwd="pw", roles=[])
    # Data the reader is allowed to aggregate over.
    client.shop.orders.insert_many([{"_id": i, "v": i} for i in range(3)])


@contextlib.contextmanager
def _python_server(tmp_path):
    path = str(tmp_path)
    with (
        SecantusDBServer(port=0, storage_path=path) as seed_srv,
        pymongo.MongoClient(seed_srv.uri, serverSelectionTimeoutMS=2000) as c,
    ):
        _seed(c)
    srv = SecantusDBServer(port=0, storage_path=path, require_auth=True)
    srv.start()
    try:
        host, port = "127.0.0.1", srv.port
        yield host, port
    finally:
        srv.stop()


@contextlib.contextmanager
def _rust_server(tmp_path):
    _server = pytest.importorskip("_secantus_server")
    path = str(tmp_path / "wt")
    seed_srv = _server.RustServer(path, 0)
    try:
        host, port = seed_srv.address
        with pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
        ) as c:
            _seed(c)
    finally:
        seed_srv.stop()
    srv = _server.RustServer(path, 0, require_auth=True)
    try:
        yield srv.address
    finally:
        srv.stop()


_SERVERS = {"python": _python_server, "rust": _rust_server}


@pytest.fixture(params=["python", "rust"])
def addr(request, tmp_path):
    with _SERVERS[request.param](tmp_path) as hostport:
        yield hostport


def _connect(addr, user: str) -> pymongo.MongoClient:
    host, port = addr
    return pymongo.MongoClient(
        host,
        port,
        username=user,
        password="pw",
        authSource="admin",
        directConnection=True,
        serverSelectionTimeoutMS=5000,
    )


def _agg(client, pipeline):
    return client.shop.command("aggregate", "orders", pipeline=pipeline, cursor={})


def test_reader_can_run_plain_and_lookup_pipelines(addr) -> None:
    """The find grant still authorizes reads, including the $lookup family on
    the same db (RBAC is db-granular)."""
    with _connect(addr, "reader") as c:
        assert _agg(c, [{"$match": {}}])["ok"] == 1.0
        assert (
            _agg(
                c,
                [
                    {
                        "$lookup": {
                            "from": "customers",
                            "localField": "_id",
                            "foreignField": "orderId",
                            "as": "x",
                        }
                    }
                ],
            )["ok"]
            == 1.0
        )


@pytest.mark.parametrize(
    "stage",
    [
        {"$out": "stolen"},
        {"$out": {"db": "warehouse", "coll": "stolen"}},
        {"$merge": {"into": "stolen"}},
        {"$merge": {"into": {"db": "warehouse", "coll": "stolen"}}},
    ],
    ids=["out-same-db", "out-cross-db", "merge-same-db", "merge-cross-db"],
)
def test_reader_cannot_write_via_out_or_merge(addr, stage) -> None:
    """find-only must not authorize $out/$merge writes — same or cross db."""
    with _connect(addr, "reader") as c:
        with pytest.raises(OperationFailure) as exc:
            _agg(c, [stage])
        assert exc.value.code == 13


def test_out_denied_inside_facet_or_subpipeline_walk(addr) -> None:
    """The requirement walk descends into $unionWith sub-pipelines, so a
    nested writing stage can't hide from the check. (A nested $out is not
    valid MongoDB — but the authorization must deny before any validation
    or execution gets a chance to touch the target namespace.)"""
    with _connect(addr, "reader") as c:
        with pytest.raises(OperationFailure) as exc:
            _agg(
                c,
                [
                    {
                        "$unionWith": {
                            "coll": "orders",
                            "pipeline": [{"$merge": {"into": "stolen"}}],
                        }
                    }
                ],
            )
        assert exc.value.code == 13


def test_configure_failpoint_requires_cluster_admin(addr) -> None:
    """#806: reader and zero-role principals get Unauthorized; a
    clusterAdmin-bound user may arm (and disarm) a failpoint."""
    for user in ("reader", "norole"):
        with _connect(addr, user) as c:
            with pytest.raises(OperationFailure) as exc:
                c.admin.command("configureFailPoint", "failCommand", mode="off")
            assert exc.value.code == 13
    with _connect(addr, "opsadmin") as c:
        assert c.admin.command("configureFailPoint", "failCommand", mode="off")["ok"] == 1.0
