"""Gap tests: one or more per open item in a Rust server's backlog.

A gap test asserts what the REAL product does -- mongod 8.2.11 or
PostgreSQL 15 -- for the behaviour a backlog item says is missing, so it
FAILS while the item is open and passes once it is fixed. Each is marked
``@pytest.mark.backlog("RM-012")`` with the item's ID, and the item's
``Verify:`` line names it. ``python tools/backlog.py verify`` runs them.

They are excluded from the default suite by their marker (the open work is
the reason they fail, not a defect in the suite), and a gap test without
the marker is a collection error here.

When an item is fixed its gap test stays here as a REGRESSION test, re-marked
``@pytest.mark.backlog_fixed("RM-012")``: it must pass, and the verifier
reports it REGRESSED if it ever fails. The ID stays on it so a fix can be
traced back to the item and the test that proves it.

Write the assertion so that ANY departure from the real product fails on
an ``assert``: compare ``pg_answer`` / ``mongo_answer`` results, which turn
a server error into a value. A gap test that fails any other way (a crash,
a fixture error) is reported BROKEN, not OPEN -- it is not measuring the gap.
Record the reference's answer as a literal where it is stable (measured
once, with the version in a comment), or ask a live reference fixture.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import threading
import time
from collections.abc import Callable, Iterator
from typing import Any

import pytest

psycopg = pytest.importorskip("psycopg")
pymongo = pytest.importorskip("pymongo")

REPO = pathlib.Path(__file__).resolve().parents[2]
RS_BINARY = pathlib.Path(
    os.environ.get("SECANTUSDB_BIN") or REPO / "crates/secantusdb/target/debug/secantusd-rs"
)
_RS_BANNER = re.compile(r"secantusd-rs listening on (\S+):(\d+)")
PG15_DSN = os.environ.get(
    "SECANTUS_PG_ORACLE_DSN_15", "host=127.0.0.1 port=5415 dbname=postgres user=postgres"
)


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    here = pathlib.Path(__file__).parent
    for item in items:
        marked = list(item.iter_markers("backlog")) or list(item.iter_markers("backlog_fixed"))
        if pathlib.Path(str(item.fspath)).parent == here and not marked:
            raise pytest.UsageError(
                f"{item.nodeid}: needs @pytest.mark.backlog(ID) or @pytest.mark.backlog_fixed(ID)"
            )
    # `tools/backlog.py verify --file F --regressions` runs one server's
    # tests: SECANTUS_BACKLOG_PREFIXES=RM keeps only items whose IDs carry
    # that prefix, so the MongoDB run needs no PostgreSQL reference.
    # SECANTUS_BACKLOG_SKIP=RM-nnn drops the gap tests of the items `verify
    # --skip` names (say, one that needs more memory than a runner has).
    prefixes = tuple(p for p in os.environ.get("SECANTUS_BACKLOG_PREFIXES", "").split(",") if p)
    skip = {s for s in os.environ.get("SECANTUS_BACKLOG_SKIP", "").split(",") if s}
    if not prefixes and not skip:
        return
    keep, drop = [], []
    for item in items:
        ids = [
            a
            for name in ("backlog", "backlog_fixed")
            for m in item.iter_markers(name)
            for a in m.args
        ]
        gap_ids = {a for m in item.iter_markers("backlog") for a in m.args}
        wanted = not ids or not prefixes or any(i.startswith(prefixes) for i in ids)
        (keep if wanted and not skip & gap_ids else drop).append(item)
    if drop:
        config.hook.pytest_deselected(items=drop)
        items[:] = keep


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo) -> Iterator[None]:
    outcome = yield
    report = outcome.get_result()
    path = os.environ.get("SECANTUS_BACKLOG_REPORT")
    marked = list(item.iter_markers("backlog")) or list(item.iter_markers("backlog_fixed"))
    if not path or not marked:
        return
    # One record per test: the call phase, or a setup phase that failed.
    if report.when == "call" or (report.when == "setup" and not report.passed):
        exc = call.excinfo
        rec = {
            "nodeid": item.nodeid,
            "outcome": report.outcome if report.when == "call" else "error",
            "assertion": bool(exc and exc.errisinstance(AssertionError)) and report.when == "call",
            "message": str(exc.value).splitlines()[0][:300] if exc and str(exc.value) else "",
            "ids": [a for m in item.iter_markers("backlog") for a in m.args],
            "fixed_ids": [a for m in item.iter_markers("backlog_fixed") for a in m.args],
        }
        with open(path, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec) + "\n")


# --------------------------------------------------------------- answers --


def pg_answer(conn: Any, sql: str) -> tuple:
    """`("ok", rows)` or `("err", sqlstate, first line of the message)`."""
    try:
        cur = conn.execute(sql)
        return ("ok", cur.fetchall() if cur.description else cur.statusmessage)
    except psycopg.Error as e:
        if not conn.autocommit:
            conn.rollback()
        return ("err", e.sqlstate, str(e).splitlines()[0] if str(e) else "")


def mongo_answer(fn: Callable[[], Any]) -> tuple:
    """`("ok", result)` or `("err", code, errmsg)` for a pymongo call."""
    try:
        return ("ok", fn())
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code, (e.details or {}).get("errmsg", str(e)))


# -------------------------------------------------------------- servers --


def drain_output(proc: subprocess.Popen) -> None:
    """Keep reading a server's output once its banner is read. Nobody reads
    the pipe otherwise, and once its 64 KiB buffer fills, every thread that
    logs blocks on the write: commands hang, and so does shutdown."""
    out = proc.stdout
    assert out is not None
    threading.Thread(target=lambda: [None for _ in out], daemon=True).start()


@pytest.fixture(scope="module")
def rust_mongo(tmp_path_factory: pytest.TempPathFactory) -> Iterator[Any]:
    """A pymongo client to a `secantusd-rs` of this module's own."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built (./inv rust-binary-build)"
    store = tmp_path_factory.mktemp("rust-mongo")
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        # A store that recovers says what it replayed before it listens.
        line, m = "", None
        for _ in range(20):
            line = proc.stdout.readline()
            m = _RS_BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs did not start: {line!r}"
        drain_output(proc)
        client = pymongo.MongoClient(
            host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
        )
        yield client
        client.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()


@pytest.fixture(scope="module")
def mongod() -> Iterator[Any]:
    """A pymongo client to a throwaway mongod (the reference). Errors -- never
    skips -- when there is none: a gap test without its reference is BROKEN."""
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod()
    client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
    try:
        yield client
    finally:
        client.close()
        _stop_mongod(proc, dbpath)


@pytest.fixture(scope="module")
def mongod_rs() -> Iterator[Any]:
    """A throwaway single-node replica set: the reference for change streams
    and retryable writes, which a standalone mongod refuses."""
    from tests.mongod_reference import MONGOD, _host_port, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=("--replSet", "backlog"))
    try:
        boot = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        host, port = _host_port(uri)
        boot.admin.command(
            {
                "replSetInitiate": {
                    "_id": "backlog",
                    "members": [{"_id": 0, "host": f"{host}:{port}"}],
                }
            }
        )
        deadline = time.monotonic() + 60
        while not boot.admin.command("hello").get("isWritablePrimary"):
            assert time.monotonic() < deadline, "the single-node set never became primary"
            time.sleep(0.2)
        boot.close()
        client = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        yield client
        client.close()
    finally:
        _stop_mongod(proc, dbpath)


@pytest.fixture(scope="module")
def rust_pg(tmp_path_factory: pytest.TempPathFactory) -> Iterator[Any]:
    """A running `secantusd-pg` of this module's own (`.connect()`)."""
    from tests.test_rust_pgserver_slice import _Server

    with _Server(tmp_path_factory.mktemp("rust-pg")) as server:
        yield server


@pytest.fixture
def pg(rust_pg: Any) -> Iterator[Any]:
    """An autocommit connection to this module's `secantusd-pg`."""
    with rust_pg.connect() as conn:
        yield conn


@pytest.fixture
def pg15() -> Iterator[Any]:
    """An autocommit connection to the PostgreSQL 15 reference."""
    try:
        conn = psycopg.connect(PG15_DSN, autocommit=True, connect_timeout=5)
    except psycopg.Error as e:
        # An error, not a skip: a gap test without its reference is BROKEN.
        raise RuntimeError(f"PostgreSQL 15 reference unreachable at {PG15_DSN}: {e}") from e
    with conn:
        yield conn
