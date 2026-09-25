"""``failCommand`` scoping and test-command advertisement, on BOTH servers.

Driver spec tests configure failpoints scoped to one client by ``appName`` --
often on that client's ``hello``, with ``closeConnection`` -- and rely on the
runner's own client staying usable to switch it off again. Before scoping, such
a failpoint reached every connection and wedged the server for the rest of a
go-driver unified run. And while ``getParameter`` advertised
``enableTestCommands: false``, pymongo's harness skipped ~1,080 unified-spec
failpoint tests the servers can run (measured 2026-09-25).
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure, ServerSelectionTimeoutError

from secantus import SecantusDBServer
from secantus.failpoints import FailPointRegistry


@pytest.fixture(params=["python", "rust"])
def uri(request, tmp_path) -> Iterator[str]:
    if request.param == "python":
        with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
            host, port = srv.address
            yield f"mongodb://{host}:{port}/?directConnection=true"
        return
    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        yield f"mongodb://{host}:{port}/?directConnection=true"
    finally:
        srv.stop()


@pytest.fixture
def client(uri: str) -> Iterator[MongoClient]:
    mc = MongoClient(uri, serverSelectionTimeoutMS=3000)
    try:
        yield mc
    finally:
        mc.close()


def _app_client(uri: str, app_name: str, timeout_ms: int = 1000) -> MongoClient:
    return MongoClient(uri, appName=app_name, serverSelectionTimeoutMS=timeout_ms)


def test_get_parameter_advertises_enable_test_commands(client: MongoClient) -> None:
    reply = client.admin.command({"getParameter": 1, "enableTestCommands": 1})
    assert reply["enableTestCommands"] is True
    assert client.admin.command({"getParameter": "*"})["enableTestCommands"] is True


def test_app_name_scopes_to_one_client(uri: str, client: MongoClient) -> None:
    """Fires only for the named client, and another client's commands do not
    spend its ``times`` budget."""
    target = _app_client(uri, "failpointTarget", 3000)
    try:
        client.admin.command(
            {
                "configureFailPoint": "failCommand",
                "mode": {"times": 1},
                "data": {"failCommands": ["find"], "errorCode": 2, "appName": "failpointTarget"},
            }
        )
        assert list(client["fp_app"]["c"].find({})) == []
        with pytest.raises(OperationFailure) as exc:
            list(target["fp_app"]["c"].find({}))
        assert exc.value.code == 2
        assert list(target["fp_app"]["c"].find({})) == []
    finally:
        client.admin.command({"configureFailPoint": "failCommand", "mode": "off"})
        target.close()


def test_app_name_on_hello_does_not_wedge_other_clients(uri: str, client: MongoClient) -> None:
    """An ``alwaysOn`` ``closeConnection`` on ``hello`` stays with its client.

    The handshake ``hello`` is itself in scope -- it carries
    ``client.application.name`` before the connection has recorded anything --
    and pymongo sends it as legacy lower-case ``ismaster``, which the
    ``isMaster`` entry must cover.
    """
    client.admin.command(
        {
            "configureFailPoint": "failCommand",
            "mode": "alwaysOn",
            "data": {
                "failCommands": ["hello", "isMaster"],
                "closeConnection": True,
                "appName": "failingHeartbeat",
            },
        }
    )
    try:
        victim = _app_client(uri, "failingHeartbeat")
        try:
            with pytest.raises(ServerSelectionTimeoutError):
                victim.admin.command("ping")
        finally:
            victim.close()
        bystander = MongoClient(uri, serverSelectionTimeoutMS=3000)
        try:
            assert bystander.admin.command("ping")["ok"] == 1.0
        finally:
            bystander.close()
    finally:
        client.admin.command({"configureFailPoint": "failCommand", "mode": "off"})
    recovered = _app_client(uri, "failingHeartbeat", 3000)
    try:
        assert recovered.admin.command("ping")["ok"] == 1.0
    finally:
        recovered.close()


def test_registry_matches_canonical_name_of_an_alias() -> None:
    # mongod compares ``failCommands`` against the canonical command name.
    reg = FailPointRegistry()
    reg.configure("failCommand", "alwaysOn", {"failCommands": ["isMaster"], "errorCode": 2})
    assert reg.match("ismaster") is not None
    assert reg.match("hello") is None
    reg.configure("failCommand", "alwaysOn", {"failCommands": ["findAndModify"], "errorCode": 2})
    assert reg.match("findandmodify") is not None


@pytest.mark.parametrize("how", ["killAllSessions", "endSessions", "killSessions"])
def test_ending_a_session_aborts_its_open_transaction(
    uri: str, client: MongoClient, how: str
) -> None:
    """A transaction left open must not keep blocking other writers.

    Driver test runners call ``killAllSessions`` between tests to clear exactly
    this. The Rust server answered it (and ``endSessions`` / ``killSessions``)
    as a no-op, so once a failpoint test left a transaction open, every later
    write to the same document retried on ``WriteConflict`` -- 68 pymongo
    unified tests failed that way and four gauge workers died (2026-09-25).
    """
    coll = client["fp_sessions"]["c"]
    coll.insert_one({"_id": 0})  # create the collection outside the transaction
    session = client.start_session()
    session.start_transaction()
    coll.insert_one({"_id": 1}, session=session)  # left uncommitted on purpose
    if how == "killAllSessions":
        client.admin.command("killAllSessions", [])
    else:
        client.admin.command(how, [session.session_id])

    other = MongoClient(uri, serverSelectionTimeoutMS=3000)
    try:
        # With the transaction aborted this insert has nothing to conflict with.
        other["fp_sessions"]["c"].insert_one({"_id": 1})
        assert other["fp_sessions"]["c"].count_documents({}) == 2
    finally:
        other.close()
