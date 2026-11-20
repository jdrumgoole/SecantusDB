"""kill -9 the Rust servers under a write burst, many times, and check the store.

The oplog is how both Rust servers recover data after a crash, and phase 4b of
``tasks/mongod-storage-model-plan.md`` rebuilds it, so this harness is the guard
beside that work. For each server it runs ``SECANTUS_CRASH_KILLS`` cycles
(default 4; the phase gate runs 50) against ONE store:

1. start the binary on the store (the second and later starts are recoveries);
2. check the store against everything acknowledged so far;
3. run four concurrent writers (single inserts, updates, deletes, a batched
   insert, and a multi-document transaction that spans two collections), every
   write acknowledged with ``j: true`` (MongoDB) or a COMMIT (PostgreSQL);
4. ``SIGKILL`` the server at a random moment of the burst.

The check after each kill:

* every acknowledged write is present (an acknowledged delete is gone);
* no unacknowledged multi-document write is partly applied: the one batch or
  transaction each writer had in flight is whole or absent, never a part;
* an unacknowledged single write landed whole or not at all;
* nothing else is in the store.

MongoDB adds a change stream: one is opened before the writers start, and
after the kill a new one resumes from the last token it saw. Every insert that
was acknowledged in the cycle must be in the events seen before the kill or
in the resumed ones, and the resumed stream must carry no error.

A failure here is a durability bug, never a flake: do not retry or skip it.
"""

from __future__ import annotations

import collections
import contextlib
import os
import pathlib
import random
import re
import subprocess
import sys
import threading
import time
from typing import Any

import pytest

pymongo = None
psycopg = None
with contextlib.suppress(ImportError):
    import pymongo  # type: ignore[no-redef]
    from pymongo.write_concern import WriteConcern
with contextlib.suppress(ImportError):
    import psycopg  # type: ignore[no-redef]

_REPO = pathlib.Path(__file__).resolve().parent.parent
_KILLS = int(os.environ.get("SECANTUS_CRASH_KILLS", "4"))
_CLIENTS = 4
_SEED = int(os.environ.get("SECANTUS_CRASH_SEED", "0")) or random.SystemRandom().randrange(1 << 30)

pytestmark = [
    pytest.mark.skipif(sys.platform == "win32", reason="SIGKILL semantics: POSIX only"),
    # A cycle (start, check, burst, kill, stop) takes about 12 s on a debug
    # build; the default 600 s per test would cut a 50-kill run short.
    pytest.mark.timeout(300 + 40 * _KILLS),
]


def _mdb_binary() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        return pathlib.Path(env) if pathlib.Path(env).exists() else None
    for profile in ("debug", "release"):
        p = _REPO / "crates" / "secantusdb" / "target" / profile / "secantusd-mdb"
        if p.exists():
            return p
    return None


def _pg_binary() -> pathlib.Path | None:
    env = os.environ.get("SECANTUSD_PG_BIN")
    if env:
        return pathlib.Path(env) if pathlib.Path(env).exists() else None
    for profile in ("debug", "release"):
        p = _REPO / "crates" / "secantus-pg" / "target" / profile / "secantusd-pg"
        if p.exists():
            return p
    return None


class _Server:
    def __init__(self, argv: list[str], banner: re.Pattern[str]) -> None:
        # The shipped default is durable; the suite's fast-storage mode
        # (conftest) would make an acknowledged write non-durable, which is
        # not what a kill -9 test is about.
        env = {k: v for k, v in os.environ.items() if k != "SECANTUS_TEST_FAST_STORAGE"}
        self.proc = subprocess.Popen(
            argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env
        )
        assert self.proc.stdout is not None
        self.tail: collections.deque[str] = collections.deque(maxlen=200)
        m = None
        for _ in range(200):
            line = self.proc.stdout.readline()
            if not line:
                break
            self.tail.append(line)
            m = banner.search(line)
            if m:
                break
        assert m, "server did not start:\n" + "".join(self.tail)
        out = self.proc.stdout

        def _pump() -> None:
            for ln in out:
                self.tail.append(ln)

        threading.Thread(target=_pump, daemon=True).start()
        self.host, self.port = m.group(1), int(m.group(2))

    def kill(self) -> None:
        self.proc.kill()  # SIGKILL: no shutdown, no checkpoint, no log flush
        self.proc.wait(timeout=30)

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=60)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)


class _Backend:
    """What a writer needs of a server; each method returns once acknowledged."""

    def insert(self, i: int) -> None: ...
    def update(self, i: int, v: int) -> None: ...
    def delete(self, i: int) -> None: ...
    def batch(self, ids: list[int], g: int) -> None: ...
    def txn(self, ids: list[int], g: int) -> None: ...
    def close(self) -> None: ...


class _Writer:
    """Client `k`: random operations on the ids it owns, each recorded only once
    acknowledged. State persists across kill cycles; `resolve` folds in what
    the one operation in flight at a kill turned out to do."""

    def __init__(self, k: int, seed: int) -> None:
        self.k = k
        self.rng = random.Random(seed * 131 + k)
        self.counter = 0
        self.singles: dict[int, int] = {}  # id -> v, acknowledged and live
        self.groups: list[list[int]] = []  # whole, acknowledged
        self.inflight: tuple[str, list[int], Any] | None = None
        self.n_acked = 0
        self.cycle_inserts: list[int] = []  # ids acknowledged inserted this cycle
        self.error: tuple[float, BaseException] | None = None

    def _new_ids(self, n: int) -> list[int]:
        out = []
        for _ in range(n):
            self.counter += 1
            out.append(self.counter * _CLIENTS + self.k)
        return out

    def _one(self, be: _Backend) -> None:
        live = list(self.singles)
        op = self.rng.choice(
            ["insert", "insert", "update", "update", "delete", "batch", "txn", "txn"]
            if live
            else ["insert"]
        )
        if op == "insert":
            (i,) = self._new_ids(1)
            self.inflight = ("insert", [i], 0)
            be.insert(i)
            self.singles[i] = 0
            self.cycle_inserts.append(i)
        elif op == "update":
            i = self.rng.choice(live)
            self.inflight = ("update", [i], (self.singles[i], self.singles[i] + 1))
            be.update(i, self.singles[i] + 1)
            self.singles[i] += 1
        elif op == "delete":
            i = self.rng.choice(live)
            self.inflight = ("delete", [i], self.singles[i])
            be.delete(i)
            del self.singles[i]
        else:
            n = 3 if op == "txn" else 4
            ids = self._new_ids(n)
            g = ids[0]
            self.inflight = (op, ids, g)
            (be.txn if op == "txn" else be.batch)(ids, g)
            self.groups.append(ids)
            self.cycle_inserts.extend(ids)
        self.inflight = None
        self.n_acked += 1

    def run(self, be: _Backend, stop: threading.Event) -> None:
        while not stop.is_set():
            try:
                self._one(be)
            except Exception as exc:  # noqa: BLE001 - classified by the caller
                self.error = (time.monotonic(), exc)
                return

    def resolve(self, found: dict[int, tuple[int, int]]) -> list[str]:
        """Fold the in-flight operation into the state, from what the store
        holds. Returns the violations of all-or-nothing."""
        bad: list[str] = []
        if self.inflight is None:
            return bad
        kind, ids, extra = self.inflight
        present = [i for i in ids if i in found]
        if kind == "insert":
            if present:
                if found[ids[0]][0] != 0:
                    bad.append(f"in-flight insert {ids[0]} landed with {found[ids[0]]}")
                self.singles[ids[0]] = 0
        elif kind == "update":
            old, new = extra
            if ids[0] not in found:
                bad.append(f"in-flight update: {ids[0]} vanished")
            elif found[ids[0]][0] not in (old, new):
                bad.append(f"in-flight update {ids[0]}: {found[ids[0]][0]} not in {old, new}")
            else:
                self.singles[ids[0]] = found[ids[0]][0]
        elif kind == "delete":
            if ids[0] in found:
                if found[ids[0]][0] != extra:
                    bad.append(f"in-flight delete {ids[0]}: value {found[ids[0]][0]}")
            else:
                self.singles.pop(ids[0], None)
        else:  # batch / txn
            if present and len(present) != len(ids):
                bad.append(
                    f"in-flight {kind} {ids} is PARTLY applied: present {present}, "
                    f"absent {[i for i in ids if i not in found]}"
                )
            if len(present) == len(ids):
                self.groups.append(ids)
        self.inflight = None
        return bad


def _check_store(writers: list[_Writer], found: dict[int, tuple[int, int]]) -> list[str]:
    """`found`: id -> (v, g). Compare to everything acknowledged and resolved."""
    bad: list[str] = []
    for w in writers:
        bad += w.resolve(found)
    expected: dict[int, tuple[int, int | None]] = {}
    for w in writers:
        for i, v in w.singles.items():
            expected[i] = (v, None)
        for ids in w.groups:
            for i in ids:
                expected[i] = (0, ids[0])
    for i, (v, g) in expected.items():
        if i not in found:
            bad.append(f"ACKNOWLEDGED write lost: id {i} (expected v={v}, g={g})")
        else:
            fv, fg = found[i]
            if fv != v or (g is not None and fg != g):
                bad.append(f"id {i} holds {found[i]}, expected v={v} g={g}")
    stray = sorted(set(found) - set(expected))
    if stray:
        bad.append(f"documents no acknowledged write created: {stray[:20]}")
    return bad


def _burst(
    writers: list[_Writer], backends: list[_Backend], server: _Server, rng: random.Random
) -> float:
    """Run the writers, SIGKILL the server at a random moment, return the kill
    time. Fails if a writer errored while the server was still up."""
    stop = threading.Event()
    threads = [
        threading.Thread(target=w.run, args=(b, stop), daemon=True)
        for w, b in zip(writers, backends, strict=True)
    ]
    for t in threads:
        t.start()
    target = rng.choice([0, 0, 5, 20, 40])
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and sum(w.n_acked for w in writers) < target:
        assert server.proc.poll() is None, "server exited under load\n" + "".join(server.tail)
        time.sleep(0.01)
    time.sleep(rng.uniform(0.0, 0.8))
    killed_at = time.monotonic()
    server.kill()
    stop.set()
    for t in threads:
        t.join(timeout=60)
        assert not t.is_alive(), "a writer did not notice the server die"
    for w in writers:
        if w.error is not None and w.error[0] < killed_at - 0.05:
            raise AssertionError(
                f"writer {w.k} failed BEFORE the kill (not a crash artefact): {w.error[1]!r}\n"
                + "".join(server.tail)
            )
    return killed_at


# --------------------------------------------------------------------------
# MongoDB
# --------------------------------------------------------------------------


class _MongoBackend(_Backend):
    def __init__(self, server: _Server) -> None:
        self.client = pymongo.MongoClient(
            server.host,
            server.port,
            directConnection=True,
            retryWrites=False,
            serverSelectionTimeoutMS=10_000,
            socketTimeoutMS=15_000,
        )
        wc = WriteConcern(w=1, j=True)
        self.wc = wc
        self.c = self.client.crash.get_collection("c", write_concern=wc)
        self.d = self.client.crash.get_collection("d", write_concern=wc)

    def insert(self, i: int) -> None:
        self.c.insert_one({"_id": i, "v": 0})

    def update(self, i: int, v: int) -> None:
        self.c.update_one({"_id": i}, {"$set": {"v": v}})

    def delete(self, i: int) -> None:
        self.c.delete_one({"_id": i})

    def batch(self, ids: list[int], g: int) -> None:
        self.c.insert_many([{"_id": i, "v": 0, "g": g} for i in ids])

    def txn(self, ids: list[int], g: int) -> None:
        # A TransientTransactionError aborted the transaction whole: mongod's
        # contract is to run it again.
        for attempt in range(10):
            try:
                with self.client.start_session() as s:
                    s.start_transaction(write_concern=self.wc)
                    self.c.insert_many([{"_id": i, "v": 0, "g": g} for i in ids[:2]], session=s)
                    self.d.insert_one({"_id": ids[2], "v": 0, "g": g}, session=s)
                    s.commit_transaction()
                return
            except pymongo.errors.PyMongoError as exc:
                # pymongo labels a lost connection transient too; only an
                # answer from the server (OperationFailure) is worth a rerun.
                if (
                    attempt == 9
                    or not isinstance(exc, pymongo.errors.OperationFailure)
                    or not exc.has_error_label("TransientTransactionError")
                ):
                    raise

    def close(self) -> None:
        self.client.close()


def _mongo_scan(server: _Server) -> dict[int, tuple[int, int]]:
    client = pymongo.MongoClient(
        server.host, server.port, directConnection=True, serverSelectionTimeoutMS=30_000
    )
    try:
        found: dict[int, tuple[int, int]] = {}
        for coll in ("c", "d"):
            for doc in client.crash[coll].find({}):
                assert doc["_id"] not in found, f"id {doc['_id']} in both collections"
                found[doc["_id"]] = (doc["v"], doc.get("g", -1))
        return found
    finally:
        client.close()


class _Watcher(threading.Thread):
    """A change stream on the database, opened before the burst."""

    def __init__(self, server: _Server) -> None:
        super().__init__(daemon=True)
        self.client = pymongo.MongoClient(
            server.host, server.port, directConnection=True, serverSelectionTimeoutMS=10_000
        )
        self.stream = self.client.crash.watch(max_await_time_ms=100)
        self.token: Any = None
        self.inserted: set[int] = set()
        self.stopped = threading.Event()

    def run(self) -> None:
        try:
            while not self.stopped.is_set():
                ev = self.stream.try_next()
                if ev is not None:
                    self.token = ev["_id"]
                    if ev["operationType"] == "insert":
                        self.inserted.add(ev["documentKey"]["_id"])
        except Exception:  # noqa: BLE001 - the server was killed under it
            pass


def _resumed_inserts(server: _Server, token: Any) -> set[int]:
    client = pymongo.MongoClient(
        server.host, server.port, directConnection=True, serverSelectionTimeoutMS=30_000
    )
    seen: set[int] = set()
    try:
        with client.crash.watch(resume_after=token, max_await_time_ms=300) as stream:
            idle = 0
            while idle < 3:
                ev = stream.try_next()
                if ev is None:
                    idle += 1
                    continue
                idle = 0
                if ev["operationType"] == "insert":
                    seen.add(ev["documentKey"]["_id"])
    finally:
        client.close()
    return seen


@pytest.mark.skipif(pymongo is None, reason="pymongo not installed")
@pytest.mark.skipif(_mdb_binary() is None, reason="secantusd-mdb binary not built")
def test_mdb_kill_loop(tmp_path: pathlib.Path) -> None:
    binary = _mdb_binary()
    assert binary is not None
    store = tmp_path / "store"
    banner = re.compile(r"secantusd-mdb listening on (\S+):(\d+)")
    rng = random.Random(_SEED)
    writers = [_Writer(k, _SEED) for k in range(_CLIENTS)]
    boot = _Server([str(binary), "--port", "0", "--storage-path", str(store)], banner)
    try:
        # Both collections exist before the burst: two transactions creating
        # `d` at once conflict, as they do on mongod, and that is not what
        # this test is about.
        c = pymongo.MongoClient(boot.host, boot.port, directConnection=True)
        for coll in ("c", "d"):
            c.crash.create_collection(coll)
        c.close()
    finally:
        # Killed, not stopped: a new store's first life ends with no
        # checkpoint at all, and what the first cycle opens is what recovery
        # makes of the oplog and the creates alone.
        boot.kill()
    token: Any = None
    watched: set[int] = set()
    cycle_inserts: list[int] = []
    problems: list[str] = []
    try:
        for cycle in range(_KILLS):
            server = _Server([str(binary), "--port", "0", "--storage-path", str(store)], banner)
            try:
                found = _mongo_scan(server)
                bad = _check_store(writers, found)
                if cycle > 0 and token is not None:
                    resumed = _resumed_inserts(server, token)
                    missing = sorted(set(cycle_inserts) - watched - resumed)
                    # An insert the stream has not carried, or one a later
                    # delete of its own removed, is still an insert event.
                    if missing:
                        bad.append(
                            f"change stream resumed from {token} lacks acknowledged "
                            f"inserts {missing[:20]}"
                        )
                assert not bad, (
                    f"cycle {cycle} (seed {_SEED}): "
                    + "; ".join(bad[:10])
                    + "\n"
                    + "".join(server.tail)
                )
                for w in writers:
                    w.cycle_inserts = []
                backends = [_MongoBackend(server) for _ in writers]
                watcher = _Watcher(server)
                watcher.start()
                try:
                    time.sleep(0.05)
                    _burst(writers, backends, server, rng)
                finally:
                    watcher.stopped.set()
                    watcher.join(timeout=30)
                    for b in backends:
                        b.close()
                token, watched = watcher.token, set(watcher.inserted)
                cycle_inserts = [i for w in writers for i in w.cycle_inserts]
            finally:
                server.stop()
        # Last restart: the final state must be whole too.
        server = _Server([str(binary), "--port", "0", "--storage-path", str(store)], banner)
        try:
            found = _mongo_scan(server)
            problems = _check_store(writers, found)
            if token is not None:
                resumed = _resumed_inserts(server, token)
                missing = sorted(set(cycle_inserts) - watched - resumed)
                if missing:
                    problems.append(f"final resume lacks acknowledged inserts {missing[:20]}")
        finally:
            server.stop()
    finally:
        pass
    acked = sum(w.n_acked for w in writers)
    assert not problems, f"after {_KILLS} kills (seed {_SEED}, {acked} acked): {problems[:10]}"


# --------------------------------------------------------------------------
# PostgreSQL
# --------------------------------------------------------------------------


class _PgBackend(_Backend):
    def __init__(self, server: _Server) -> None:
        self.conn = psycopg.connect(
            f"host={server.host} port={server.port} dbname=postgres user=postgres",
            autocommit=True,
            connect_timeout=30,
        )

    def insert(self, i: int) -> None:
        self.conn.execute("INSERT INTO c (id, v, g) VALUES (%s, 0, -1)", (i,))

    def update(self, i: int, v: int) -> None:
        self.conn.execute("UPDATE c SET v = %s WHERE id = %s", (v, i))

    def delete(self, i: int) -> None:
        self.conn.execute("DELETE FROM c WHERE id = %s", (i,))

    def batch(self, ids: list[int], g: int) -> None:
        rows = ", ".join(f"({i}, 0, {g})" for i in ids)
        self.conn.execute(f"INSERT INTO c (id, v, g) VALUES {rows}")

    def txn(self, ids: list[int], g: int) -> None:
        with self.conn.transaction():
            for i in ids[:2]:
                self.conn.execute("INSERT INTO c (id, v, g) VALUES (%s, 0, %s)", (i, g))
            self.conn.execute("INSERT INTO d (id, v, g) VALUES (%s, 0, %s)", (ids[2], g))

    def close(self) -> None:
        with contextlib.suppress(Exception):  # the server is gone
            self.conn.close()


def _pg_scan(server: _Server) -> dict[int, tuple[int, int]]:
    conn = psycopg.connect(
        f"host={server.host} port={server.port} dbname=postgres user=postgres",
        autocommit=True,
        connect_timeout=60,
    )
    try:
        found: dict[int, tuple[int, int]] = {}
        for tbl in ("c", "d"):
            for i, v, g in conn.execute(f"SELECT id, v, g FROM {tbl}").fetchall():
                assert i not in found, f"id {i} in both tables"
                found[i] = (v, g)
        return found
    finally:
        conn.close()


@pytest.mark.skipif(psycopg is None, reason="psycopg not installed")
@pytest.mark.skipif(_pg_binary() is None, reason="secantusd-pg binary not built")
def test_pg_kill_loop(tmp_path: pathlib.Path) -> None:
    binary = _pg_binary()
    assert binary is not None
    store = tmp_path / "data"
    banner = re.compile(r"secantusd-pg listening on (\S+):(\d+)")
    rng = random.Random(_SEED)
    writers = [_Writer(k, _SEED) for k in range(_CLIENTS)]
    argv = [str(binary), str(store), "127.0.0.1:0"]

    server = _Server(argv, banner)
    try:
        conn = psycopg.connect(
            f"host={server.host} port={server.port} dbname=postgres user=postgres",
            autocommit=True,
        )
        for t in ("c", "d"):
            conn.execute(f"CREATE TABLE {t} (id int PRIMARY KEY, v int, g int)")
        conn.close()
    finally:
        # Killed, not stopped (see the MongoDB test).
        server.kill()

    for cycle in range(_KILLS):
        server = _Server(argv, banner)
        try:
            bad = _check_store(writers, _pg_scan(server))
            assert not bad, (
                f"cycle {cycle} (seed {_SEED}): "
                + "; ".join(bad[:10])
                + "\n"
                + "".join(server.tail)
            )
            backends = [_PgBackend(server) for _ in writers]
            try:
                _burst(writers, backends, server, rng)
            finally:
                for b in backends:
                    b.close()
        finally:
            server.stop()
    server = _Server(argv, banner)
    try:
        problems = _check_store(writers, _pg_scan(server))
    finally:
        server.stop()
    acked = sum(w.n_acked for w in writers)
    assert not problems, f"after {_KILLS} kills (seed {_SEED}, {acked} acked): {problems[:10]}"
