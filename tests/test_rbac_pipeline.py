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

Each test seeds users and roles on an auth-off server, restarts the same
storage directory with ``require_auth`` on, and drives the checks over real
pymongo connections.

The two servers are held to different answers where they differ, and the
Rust server's are mongod 8.2.11's (``tools/probes/auth_enforcement.py``,
2026-10-14): a ``$lookup`` needs ``find`` on the collection it reads, and
``configureFailPoint`` needs no privilege at all -- it is gated by test
commands being enabled, not by a role. The Python server keeps its
database-granular check and its cluster-admin gate on the failpoint.
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
def server(request, tmp_path):
    """``(kind, (host, port))`` of an auth-on server seeded by ``_seed``."""
    with _SERVERS[request.param](tmp_path) as hostport:
        yield request.param, hostport


@pytest.fixture
def addr(server):
    return server[1]


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


def test_reader_can_run_plain_pipelines_and_lookup_needs_the_foreign_find(server) -> None:
    """The find grant authorizes reads of the collection it names. A $lookup
    reads another collection: mongod 8.2.11 wants ``find`` on that one too
    (a user with ``find`` on ``ae.c1`` alone is refused ``$lookup`` from
    ``c2``), which the Rust server now checks; the Python server's check is
    db-granular and lets it through."""
    kind, addr = server
    with _connect(addr, "reader") as c:
        assert _agg(c, [{"$match": {}}])["ok"] == 1.0
        lookup = [
            {
                "$lookup": {
                    "from": "customers",
                    "localField": "_id",
                    "foreignField": "orderId",
                    "as": "x",
                }
            }
        ]
        if kind == "rust":
            with pytest.raises(OperationFailure) as exc:
                _agg(c, lookup)
            assert exc.value.code == 13
        else:
            assert _agg(c, lookup)["ok"] == 1.0


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


def test_configure_failpoint_gate(server) -> None:
    """#806 on the Python server: reader and zero-role principals get
    Unauthorized; a clusterAdmin-bound user may arm (and disarm) a failpoint.

    On the Rust server any authenticated user may: that is mongod 8.2.11's
    answer (every one of 62 users ran it, with test commands enabled), the
    command being gated by ``enableTestCommands`` rather than by a role."""
    kind, addr = server
    for user in ("reader", "norole"):
        with _connect(addr, user) as c:
            if kind == "rust":
                assert c.admin.command("configureFailPoint", "failCommand", mode="off")["ok"] == 1.0
            else:
                with pytest.raises(OperationFailure) as exc:
                    c.admin.command("configureFailPoint", "failCommand", mode="off")
                assert exc.value.code == 13
    with _connect(addr, "opsadmin") as c:
        assert c.admin.command("configureFailPoint", "failCommand", mode="off")["ok"] == 1.0
