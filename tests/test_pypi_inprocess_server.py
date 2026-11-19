"""The PyPI package's in-process ``MongoServer``, against the REAL extension.

``pip install SecantusDB`` runs the MongoDB server inside the Python process
by default: the launcher (``packaging/pypi/secantusdb``) drives the same
``_secantus_server`` extension this repo builds, installed in the wheel as
``secantusdb._secantus_server``. These tests point the launcher at the
extension built here, so every PR exercises the in-process mode, not only the
release smoke test (``packaging/pypi/smoke_test.py``).

What they pin is what the in-process mode promises beyond the subprocess
one: the daemon's defaults (a replica set, so change streams work; test
commands off), a store that is really closed on ``stop()`` and reopens with
its data in the same process, two servers side by side, WiredTiger's lock
refusing a second server on one path, and a forked child that neither stops
nor shuts down the parent's server.

Registered in test.yml's storage-engine lane, the only one that builds the
extension; anywhere else it skips.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

ROOT = Path(__file__).resolve().parent.parent
PYPI = ROOT / "packaging" / "pypi"

sys.path.insert(0, str(PYPI))
try:
    import secantusdb
    from secantusdb import _server as launcher
finally:
    sys.path.remove(str(PYPI))

# The wheel installs the extension as `secantusdb._secantus_server`; here it
# is the top-level module the repo's build installs. The same object either
# way: PyO3 names the module's init function after its last name component.
sys.modules.setdefault("secantusdb._secantus_server", _server)


def _client(server) -> pymongo.MongoClient:
    return pymongo.MongoClient(server.uri, serverSelectionTimeoutMS=10_000)


def test_in_process_crud_and_change_stream(tmp_path: Path) -> None:
    with secantusdb.MongoServer() as server:
        assert server.embedded and server.output == ""
        storage = server.storage_path
        client = _client(server)
        try:
            # A single-node replica set, as the daemon advertises by default:
            # pymongo accepts change streams and transactions only from one.
            hello = client.admin.command("hello")
            assert hello["setName"] == "secantus"
            coll = client.app.things
            with coll.watch() as stream:
                coll.insert_one({"_id": 1, "x": 1})
                event = stream.try_next()
                for _ in range(50):
                    if event is not None:
                        break
                    event = stream.try_next()
                assert event is not None and event["operationType"] == "insert"
                assert event["fullDocument"] == {"_id": 1, "x": 1}
            with client.start_session() as session, session.start_transaction():
                coll.insert_one({"_id": 2}, session=session)
            assert coll.count_documents({}) == 2
        finally:
            client.close()
    assert storage is not None and not storage.exists()


def test_test_commands_are_off_as_on_mongod() -> None:
    with secantusdb.MongoServer() as server:
        client = _client(server)
        try:
            with pytest.raises(pymongo.errors.OperationFailure) as info:
                client.admin.command("configureFailPoint", "failCommand", mode="off")
            # mongod without --setParameter enableTestCommands=1:
            # CommandNotFound.
            assert info.value.code == 59
        finally:
            client.close()


def test_stop_closes_the_store_and_it_reopens_in_process(tmp_path: Path) -> None:
    path = tmp_path / "data"
    with secantusdb.MongoServer(path) as server:
        client = _client(server)
        client.app.things.insert_many([{"_id": i} for i in range(100)])
        client.close()
    # Reopened in the SAME process: only works if stop() really closed the
    # WiredTiger connection and released its lock.
    with secantusdb.MongoServer(path) as server:
        client = _client(server)
        assert client.app.things.count_documents({}) == 100
        client.close()
    # A path the caller named is theirs: kept.
    assert path.is_dir()


def test_two_servers_at_once_and_a_shared_path_is_refused(tmp_path: Path) -> None:
    with secantusdb.MongoServer(tmp_path / "a") as a, secantusdb.MongoServer() as b:
        assert a.port != b.port
        ca, cb = _client(a), _client(b)
        try:
            ca.app.t.insert_one({"server": "a"})
            cb.app.t.insert_one({"server": "b"})
            assert ca.app.t.find_one({}, {"_id": 0}) == {"server": "a"}
            assert cb.app.t.find_one({}, {"_id": 0}) == {"server": "b"}
        finally:
            ca.close()
            cb.close()
        # WiredTiger's lock: the same refusal two daemons on one path get.
        with pytest.raises(launcher.ServerStartError):
            secantusdb.MongoServer(tmp_path / "a").start()
        # The first server is untouched by the refused one.
        ca = _client(a)
        assert ca.app.t.count_documents({}) == 1
        ca.close()


def test_a_port_in_use_raises_server_start_error() -> None:
    with secantusdb.MongoServer() as server:
        second = secantusdb.MongoServer(port=server.port)
        with pytest.raises(launcher.ServerStartError, match="bind"):
            second.start()
        # Its temporary store was removed on the way out.
        assert second.storage_path is not None and not second.storage_path.exists()


_FORK_SCRIPT = textwrap.dedent(
    """
    import os, sys
    import _secantus_server
    sys.modules["secantusdb._secantus_server"] = _secantus_server
    sys.path.insert(0, sys.argv[1])
    import pymongo
    from secantusdb import ForkedServerError, MongoServer

    server = MongoServer().start()
    pymongo.MongoClient(server.uri).app.t.insert_one({"_id": 1})
    pid = os.fork()
    if pid == 0:
        # The child: it reaches the parent's server over TCP...
        client = pymongo.MongoClient(server.uri, serverSelectionTimeoutMS=10000)
        assert client.app.t.count_documents({}) == 1
        client.close()
        # ...cannot stop it...
        try:
            server.stop()
        except ForkedServerError:
            pass
        else:
            sys.exit("stop() in the forked child did not raise")
        # Leaving the parent's `with` block in the child is a no-op.
        server.__exit__(None, None, None)
        # ...and exits NORMALLY, so the interpreter finalizes and drops its
        # copy of the handle. Dropping it must not shut anything down.
        sys.exit(0)
    _, status = os.waitpid(pid, 0)
    assert os.waitstatus_to_exitcode(status) == 0, status
    # The parent's server still serves, and stops cleanly.
    client = pymongo.MongoClient(server.uri, serverSelectionTimeoutMS=10000)
    client.app.t.insert_one({"_id": 2})
    assert client.app.t.count_documents({}) == 2
    client.close()
    storage = server.storage_path
    server.stop()
    assert not storage.exists()
    print("fork OK")
    """
)


@pytest.mark.skipif(not hasattr(os, "fork"), reason="no fork() on this platform")
def test_a_forked_child_leaves_the_parents_server_alone(tmp_path: Path) -> None:
    # In a subprocess, so the child's normal exit -- interpreter
    # finalization included -- happens outside pytest's own process.
    script = tmp_path / "fork_check.py"
    script.write_text(_FORK_SCRIPT)
    result = subprocess.run(
        [sys.executable, "-W", "ignore::DeprecationWarning", str(script), str(PYPI)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "fork OK" in result.stdout
