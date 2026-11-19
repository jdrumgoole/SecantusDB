"""Smoke-test an installed SecantusDB wheel.

Two shapes, matching the two wheels per platform:

* default -- the ``cp310-abi3`` wheel: the in-process ``MongoServer`` (CRUD, a
  change stream, two servers at once, stop and reopen in one process, a start
  failure, a forked child), the subprocess ``MongoServer(embedded=False)``,
  the PostgreSQL server and both console scripts. Needs ``pymongo`` and
  ``psycopg[binary]`` installed beside the wheel.
* ``--fallback`` -- the ``py3-none`` wheel, which pip picks where the
  extension cannot load (free-threaded CPython): ``MongoServer()`` raises,
  naming ``embedded=False``, and ``embedded=False`` serves. Standard library
  only, so it needs no driver wheel for that Python.

Run with the venv's own interpreter, from outside the checkout. Exits
non-zero on any failure.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

import secantusdb
from secantusdb import MongoServer, PostgresServer, ServerStartError


def _has_extension() -> bool:
    return any(Path(secantusdb.__file__).parent.glob("_secantus_server*"))


def fallback() -> None:
    assert not _has_extension(), "the fallback wheel carries the extension"
    try:
        MongoServer().start()
    except ServerStartError as exc:
        assert "embedded=False" in str(exc), exc
        print("in-process refused, as it should be:", exc)
    else:
        raise AssertionError("MongoServer() started without the extension")
    with MongoServer(embedded=False) as server:
        with socket.create_connection((server.host, server.port), timeout=10):
            pass
        print("MongoDB server OK as a child process:", server.uri)


def full() -> None:
    import psycopg
    import pymongo

    assert _has_extension(), "the abi3 wheel has no extension: pip picked the wrong wheel"

    def client(server: MongoServer) -> pymongo.MongoClient:
        return pymongo.MongoClient(server.uri, serverSelectionTimeoutMS=10000)

    for embedded in (True, False):
        with MongoServer(embedded=embedded) as server:
            c = client(server)
            c.smoke.things.insert_one({"x": 1})
            assert c.smoke.things.find_one({}, {"_id": 0}) == {"x": 1}
            # A replica set in both modes: change streams need one.
            assert c.admin.command("hello")["setName"] == "secantus"
            with c.smoke.things.watch() as stream:
                c.smoke.things.insert_one({"x": 2})
                event = None
                for _ in range(50):
                    event = stream.try_next()
                    if event is not None:
                        break
                assert event is not None and event["fullDocument"]["x"] == 2, event
            c.close()
            storage = server.storage_path
            mode = "in-process" if embedded else "child process"
            print(f"MongoDB server OK ({mode}):", server.uri)
        assert storage is not None and not storage.exists(), f"{storage} was not removed"

    # In-process only: two at once; stop and reopen in one process.
    kept = Path(tempfile.mkdtemp(prefix="secantusdb-smoke-"))
    try:
        with MongoServer(kept / "a") as a, MongoServer() as b:
            assert a.port != b.port
            client(a).app.t.insert_many([{"_id": i} for i in range(10)])
            client(b).app.t.insert_one({"_id": "b"})
            # WiredTiger's lock refuses a second server on one path.
            try:
                MongoServer(kept / "a").start()
            except ServerStartError:
                pass
            else:
                raise AssertionError("a second server opened a locked store")
            # A port in use is a ServerStartError, not a bare native error.
            try:
                MongoServer(port=a.port).start()
            except ServerStartError:
                pass
            else:
                raise AssertionError("a second server bound a port in use")
        with MongoServer(kept / "a") as a:
            assert client(a).app.t.count_documents({}) == 10
        print("in-process: two servers, lock, reopen OK")

        # A forked child reaches the parent's server but cannot stop it, and
        # its normal exit leaves the parent's server serving.
        with MongoServer() as server:
            pid = os.fork()
            if pid == 0:
                code = 0
                try:
                    assert client(server).app.t.count_documents({}) == 0
                    try:
                        server.stop()
                        code = 3
                    except secantusdb.ForkedServerError:
                        pass
                except BaseException:
                    code = 4
                # Never unwind into the parent's cleanup below.
                os._exit(code)
            _, status = os.waitpid(pid, 0)
            assert os.waitstatus_to_exitcode(status) == 0, status
            client(server).app.t.insert_one({"after": "fork"})
        print("in-process: fork OK")
    finally:
        shutil.rmtree(kept, ignore_errors=True)

    with PostgresServer() as server, psycopg.connect(server.dsn, autocommit=True) as conn:
        row = conn.execute("select 1 + 1").fetchone()
        assert row == (2,), row
        print("PostgreSQL server OK:", server.dsn)

    scripts = Path(sys.executable).parent
    for command in ("secantusd-mdb", "secantusd-pg"):
        out = subprocess.run(
            [os.fspath(scripts / command), "--version"],
            capture_output=True,
            text=True,
            check=True,
            timeout=60,
        ).stdout
        assert command in out, out
        print(out.strip().splitlines()[0])


if __name__ == "__main__":
    fallback() if "--fallback" in sys.argv[1:] else full()
