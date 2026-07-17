"""Aggressive concurrency suite for the embedded Rust server.

Many pymongo clients (one per thread, real TCP) hammer a single ``RustServer``
with barrier-synchronized writes. Every test asserts a hard integrity
invariant: exact final counts, no lost ``$inc`` updates, unique findAndModify
tickets, exactly-one-winner unique-index races, readers that never observe a
missing stable document while the collection churns around them. The only
error a loser may see is the typed signal mongod would send (11000
DuplicateKey, 112 WriteConflict) — anything else fails the test.

Lifecycle stress (many servers started/stopped concurrently) lives in
``test_rust_server_stress.py``; this file is about contention *within* one
server. Gated on the WiredTiger-linking ``_secantus_server`` build, like the
other rust-server suites.
"""

from __future__ import annotations

import threading
import time

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

WORKERS = 8

#: The retriable per-statement conflict signals a real mongod can surface.
WRITE_CONFLICT = 112
DUPLICATE_KEY = 11000


@pytest.fixture
def server(tmp_path):
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


def make_client(srv):
    host, port = srv.address
    return pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)


def run_workers(n: int, target) -> None:
    """Run ``target(i)`` on ``n`` threads; re-raise the first worker failure."""
    failures: list[BaseException] = []

    def guarded(i: int) -> None:
        try:
            target(i)
        except BaseException as exc:  # noqa: BLE001 — surface into pytest
            failures.append(exc)

    threads = [threading.Thread(target=guarded, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    if failures:
        raise failures[0]


def error_code(exc: pymongo.errors.PyMongoError) -> int | None:
    code = getattr(exc, "code", None)
    if code is not None:
        return code
    details = getattr(exc, "details", None) or {}
    write_errors = details.get("writeErrors") or [{}]
    return write_errors[0].get("code")


def retrying(op, *, allowed=(WRITE_CONFLICT,), seen: set | None = None):
    """Run ``op()`` until it succeeds; only ``allowed`` codes may be retried."""
    while True:
        try:
            return op()
        except pymongo.errors.PyMongoError as exc:
            code = error_code(exc)
            if seen is not None:
                seen.add(code)
            if code not in allowed:
                raise


# --------------------------------------------------------------------------- #
# write storms


def test_parallel_insert_many_lands_exactly(server):
    docs_per, batches = 25, 2
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        client = make_client(server)
        coll = client["app"]["c"]
        barrier.wait()
        for b in range(batches):
            base = (i * batches + b) * docs_per
            coll.insert_many([{"_id": base + k, "who": i} for k in range(docs_per)])
        client.close()

    run_workers(WORKERS, worker)
    client = make_client(server)
    total = WORKERS * batches * docs_per
    assert client["app"]["c"].count_documents({}) == total
    ids = [d["_id"] for d in client["app"]["c"].find({}, {"_id": 1})]
    assert len(set(ids)) == total
    client.close()


def test_concurrent_inc_loses_no_updates(server):
    per_worker = 50
    client = make_client(server)
    client["app"]["c"].insert_one({"_id": 1, "n": 0})
    seen_codes: set[int | None] = set()
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        barrier.wait()
        for _ in range(per_worker):
            retrying(lambda: coll.update_one({"_id": 1}, {"$inc": {"n": 1}}), seen=seen_codes)
        cl.close()

    run_workers(WORKERS, worker)
    assert client["app"]["c"].find_one({"_id": 1})["n"] == WORKERS * per_worker
    assert seen_codes <= {WRITE_CONFLICT}, f"non-conflict errors surfaced: {seen_codes}"
    client.close()


@pytest.mark.xfail(
    reason="Rust findAndModify is find + update + re-find across separate storage "
    "calls (crates/secantus-commands/src/findandmodify.rs module caveat): a "
    "concurrent writer can land between the update and the post-image re-read, "
    "so two clients can be handed the same ticket. The Python server fixed this "
    "with Storage.update_matching(return_post_images=True); the Rust storage "
    "needs the same primitive — see tasks/backlog.md.",
    strict=False,
)
def test_findandmodify_tickets_are_unique_and_complete(server):
    per_worker = 25
    client = make_client(server)
    client["app"]["c"].insert_one({"_id": "counter", "n": 0})
    tickets: list[int] = []
    lock = threading.Lock()
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        got: list[int] = []
        barrier.wait()
        for _ in range(per_worker):
            doc = retrying(
                lambda: coll.find_one_and_update(
                    {"_id": "counter"},
                    {"$inc": {"n": 1}},
                    return_document=pymongo.ReturnDocument.AFTER,
                )
            )
            got.append(doc["n"])
        with lock:
            tickets.extend(got)
        cl.close()

    run_workers(WORKERS, worker)
    total = WORKERS * per_worker
    assert sorted(tickets) == list(range(1, total + 1)), "tickets were lost or duplicated"
    client.close()


def test_concurrent_upserts_produce_one_doc(server):
    per_worker = 20
    barrier = threading.Barrier(WORKERS)
    seen_codes: set[int | None] = set()

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        barrier.wait()
        for _ in range(per_worker):
            retrying(
                lambda: coll.update_one({"_id": "k"}, {"$inc": {"n": 1}}, upsert=True),
                allowed=(WRITE_CONFLICT, DUPLICATE_KEY),
                seen=seen_codes,
            )
        cl.close()

    run_workers(WORKERS, worker)
    client = make_client(server)
    assert client["app"]["c"].count_documents({}) == 1
    assert client["app"]["c"].find_one({"_id": "k"})["n"] == WORKERS * per_worker
    assert seen_codes <= {WRITE_CONFLICT, DUPLICATE_KEY}
    client.close()


def test_unique_index_race_has_single_winner_per_value(server):
    rounds = 20
    client = make_client(server)
    coll0 = client["app"]["c"]
    coll0.insert_one({"_id": "seed"})  # create the collection
    coll0.create_index([("u", 1)], unique=True)
    barrier = threading.Barrier(WORKERS)
    wins = [0] * WORKERS
    loser_codes: set[int | None] = set()
    lock = threading.Lock()

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        for r in range(rounds):
            barrier.wait()
            try:
                coll.insert_one({"_id": f"{r}-{i}", "u": r})
                wins[i] += 1
            except pymongo.errors.PyMongoError as exc:
                with lock:
                    loser_codes.add(error_code(exc))
        cl.close()

    run_workers(WORKERS, worker)
    assert sum(wins) == rounds, f"want exactly one winner per value, got {sum(wins)}"
    assert loser_codes == {DUPLICATE_KEY}, f"losers must see DuplicateKey: {loser_codes}"
    stored = list(coll0.find({"u": {"$exists": True}}))
    assert len(stored) == rounds
    assert len({d["u"] for d in stored}) == rounds, "duplicate unique values were stored"
    client.close()


# --------------------------------------------------------------------------- #
# mixed read/write workloads


def test_readers_see_every_stable_doc_while_collection_churns(server):
    stable_n, churn_cycles, reader_scans = 300, 30, 8
    client = make_client(server)
    coll0 = client["app"]["c"]
    coll0.insert_many([{"_id": f"s{k}", "kind": "stable"} for k in range(stable_n)])
    stable_ids = {f"s{k}" for k in range(stable_n)}
    churn_n, reader_n = 3, 4

    def churner(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        for cycle in range(churn_cycles):
            coll.insert_many([{"_id": f"c{i}-{cycle}-{k}", "kind": "churn"} for k in range(10)])
            coll.delete_many({"kind": "churn", "_id": {"$regex": f"^c{i}-{cycle}-"}})
        cl.close()

    def reader(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        for _ in range(reader_scans):
            # batch_size forces getMore round-trips mid-churn.
            seen = {d["_id"] for d in coll.find({}, batch_size=50) if d["kind"] == "stable"}
            missing = stable_ids - seen
            assert not missing, f"scan lost {len(missing)} stable docs, e.g. {sorted(missing)[:3]}"
        cl.close()

    def worker(i: int) -> None:
        (churner if i < churn_n else reader)(i)

    run_workers(churn_n + reader_n, worker)
    assert coll0.count_documents({"kind": "stable"}) == stable_n
    assert coll0.count_documents({"kind": "churn"}) == 0
    client.close()


def test_index_build_under_write_load_stays_consistent(server):
    pre_n, post_n = 200, 200
    client = make_client(server)
    coll0 = client["app"]["c"]
    coll0.insert_many([{"_id": k, "x": k % 10} for k in range(pre_n)])
    writers = 4
    started = threading.Event()

    def writer(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        started.wait()
        per = post_n // writers
        base = pre_n + i * per
        for k in range(per):
            coll.insert_one({"_id": base + k, "x": (base + k) % 10})
        cl.close()

    def builder(i: int) -> None:
        started.set()
        coll0.create_index([("x", 1)])

    def worker(i: int) -> None:
        (builder if i == 0 else writer)(i)

    run_workers(writers + 1, worker)
    total = pre_n + post_n
    assert coll0.count_documents({}) == total
    assert any(ix["name"].startswith("x_") for ix in coll0.list_indexes())
    # The index (if chosen) and a collection scan must agree for every bucket.
    for bucket in range(10):
        want = sum(1 for k in range(total) if k % 10 == bucket)
        assert coll0.count_documents({"x": bucket}) == want
    client.close()


def test_parallel_writers_on_distinct_collections(server):
    docs_per = 100

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl[f"db{i % 2}"][f"c{i}"]
        coll.insert_many([{"_id": k, "who": i} for k in range(docs_per)])
        coll.delete_many({"_id": {"$gte": docs_per - 10}})
        cl.close()

    run_workers(WORKERS, worker)
    client = make_client(server)
    for i in range(WORKERS):
        coll = client[f"db{i % 2}"][f"c{i}"]
        assert coll.count_documents({}) == docs_per - 10, f"collection c{i} count is wrong"
        assert coll.count_documents({"who": {"$ne": i}}) == 0, f"foreign docs leaked into c{i}"
    client.close()


def test_delete_insert_churn_settles_to_exact_count(server):
    inserts, deletes = 40, 20
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        barrier.wait()
        for k in range(inserts):
            coll.insert_one({"_id": f"{i}-{k}", "who": i, "k": k})
        deleted = coll.delete_many({"who": i, "k": {"$lt": deletes}}).deleted_count
        assert deleted == deletes
        cl.close()

    run_workers(WORKERS, worker)
    client = make_client(server)
    assert client["app"]["c"].count_documents({}) == WORKERS * (inserts - deletes)
    client.close()


def test_client_connection_churn_under_write_load(server):
    churners, cycles, writer_rows = 16, 6, 150
    done = threading.Event()

    def background_writer() -> None:
        cl = make_client(server)
        coll = cl["app"]["c"]
        for k in range(writer_rows):
            coll.insert_one({"_id": k})
        cl.close()
        done.set()

    bg = threading.Thread(target=background_writer)
    bg.start()

    def churner(i: int) -> None:
        for _ in range(cycles):
            cl = make_client(server)
            cl.admin.command("ping")
            n = cl["app"]["c"].count_documents({})
            assert 0 <= n <= writer_rows
            cl.close()

    try:
        run_workers(churners, churner)
    finally:
        bg.join()
    client = make_client(server)
    assert client["app"]["c"].count_documents({}) == writer_rows
    client.close()


def test_change_stream_sees_every_concurrent_insert(tmp_path):
    # Needs the replica-set persona for $changeStream.
    srv = _server.RustServer(
        str(tmp_path / "wt-cs"), 0, host="127.0.0.1", replica_set_name="secantus"
    )
    try:
        writer_n, docs_per = 4, 25
        client = make_client(srv)
        coll0 = client["csdb"]["c"]
        coll0.insert_one({"_id": "seed"})  # create the collection

        cs = coll0.watch(max_await_time_ms=200)
        try:

            def writer(i: int) -> None:
                cl = make_client(srv)
                coll = cl["csdb"]["c"]
                for k in range(docs_per):
                    coll.insert_one({"_id": f"{i}-{k}", "who": i})
                cl.close()

            run_workers(writer_n, writer)

            want = writer_n * docs_per
            got: set[str] = set()
            deadline = time.monotonic() + 30
            while len(got) < want and time.monotonic() < deadline:
                ev = cs.try_next()
                if ev is None:
                    continue
                if ev["operationType"] == "insert" and ev["documentKey"]["_id"] != "seed":
                    got.add(ev["documentKey"]["_id"])
            assert len(got) == want, f"change stream saw {len(got)} of {want} inserts"
        finally:
            cs.close()
        client.close()
    finally:
        srv.stop()
