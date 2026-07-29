"""Aggressive concurrency harness for the Mongo-wire servers — Python AND Rust.

One suite, parametrized over both server implementations: every test runs
against the pure-Python ``SecantusDBServer`` and the embedded Rust server
through real pymongo clients (one per thread, real TCP), with
barrier-synchronized writes to maximize simultaneous arrivals. The SQL/PG
analogue lives in ``test_pgserver_concurrency.py`` — the two harnesses are
deliberately separate: different wire protocols, different drivers, different
error vocabularies.

Every test asserts a hard integrity invariant: exact final counts, no lost
``$inc`` updates, unique findAndModify tickets, exactly-one-winner
unique-index races, readers that never observe a missing stable document
while the collection churns around them. The only error a loser may see is
the typed signal mongod would send (11000 DuplicateKey, 112 WriteConflict) —
anything else fails the test.

findAndModify is the most contention-sensitive command here and both servers
must pass all of it: the ``new: true`` post-image comes from the write itself
(Python ``Storage.update_matching(return_post_images=True)``, Rust
``UpdateOutcome::post_image``), and the write re-asserts the original query
keyed by the matched ``_id`` in a re-pick loop — so ticket dispensers never
hand out duplicates, and job-queue claims (``state: "new"`` → ``"taken"``)
are exclusive even when every worker races for the same document.

Rust-server lifecycle stress (many servers started/stopped concurrently)
lives in ``test_rust_server_stress.py``; this file is about contention
*within* one server. The Rust params skip when the WiredTiger-linking
``_secantus_server`` build is absent, like the other rust-server suites.
"""

from __future__ import annotations

import threading
import time

import pytest

pymongo = pytest.importorskip("pymongo")

from secantus.server import SecantusDBServer  # noqa: E402

WORKERS = 8

#: The retriable per-statement conflict signals a real mongod can surface.
WRITE_CONFLICT = 112
DUPLICATE_KEY = 11000

BOTH_SERVERS = ["python", "rust"]


@pytest.fixture(params=BOTH_SERVERS)
def kind(request):
    if request.param == "rust":
        pytest.importorskip("_secantus_server")
    return request.param


@pytest.fixture
def server(kind, tmp_path):
    if kind == "rust":
        import _secantus_server

        # The replica-set persona so change streams work, matching the Python
        # server's default hello reply.
        srv = _secantus_server.RustServer(
            str(tmp_path / "wt"), 0, host="127.0.0.1", replica_set_name="secantus"
        )
        try:
            yield srv
        finally:
            srv.stop()
    else:
        srv = SecantusDBServer(host="127.0.0.1", port=0, storage_path=str(tmp_path / "wt"))
        srv.start()
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
    assert client["app"]["c"].find_one({"_id": "counter"})["n"] == total
    client.close()


def test_findandmodify_job_queue_claims_are_exclusive(server):
    # The job-queue pattern: every worker races find_one_and_update(
    # {state: "new"}, {$set: {state: taken-by-me}}) for the same small pool.
    # The write must re-assert the query at write time — otherwise two workers
    # that both picked job J both "take" it and one worker's claim is silently
    # overwritten. Every job must end up claimed by exactly one worker, and
    # the sum of claims must equal the job count.
    jobs = 40
    client = make_client(server)
    coll0 = client["app"]["jobs"]
    coll0.insert_many([{"_id": j, "state": "new"} for j in range(jobs)])
    claims: dict[int, list[int]] = {}
    lock = threading.Lock()
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["jobs"]
        mine: list[int] = []
        barrier.wait()
        while True:
            job = retrying(
                lambda: coll.find_one_and_update(
                    {"state": "new"},
                    {"$set": {"state": "taken", "by": i}},
                    return_document=pymongo.ReturnDocument.AFTER,
                )
            )
            if job is None:
                break
            assert job["by"] == i, f"worker {i} was handed worker {job['by']}'s claim"
            mine.append(job["_id"])
        with lock:
            claims[i] = mine
        cl.close()

    run_workers(WORKERS, worker)
    claimed = [j for mine in claims.values() for j in mine]
    assert sorted(claimed) == list(range(jobs)), "jobs were double-claimed or lost"
    by_field = {d["_id"]: d["by"] for d in coll0.find({})}
    for worker_id, mine in claims.items():
        for job_id in mine:
            assert by_field[job_id] == worker_id, (
                f"job {job_id} claimed by {worker_id} but stored by={by_field[job_id]}"
            )
    client.close()


def test_findandmodify_remove_claims_are_exclusive(server):
    # fam remove=true: every worker races to remove docs from a shared pool;
    # each removed pre-image may be handed to exactly one worker (the delete
    # checks its deleted-count and re-picks on 0, so two removes can never
    # both claim the same doc).
    docs = 40
    client = make_client(server)
    coll0 = client["app"]["pool"]
    coll0.insert_many([{"_id": j} for j in range(docs)])
    removed: list[int] = []
    lock = threading.Lock()
    barrier = threading.Barrier(WORKERS)

    def worker(i: int) -> None:
        cl = make_client(server)
        coll = cl["app"]["pool"]
        mine: list[int] = []
        barrier.wait()
        while True:
            doc = retrying(lambda: coll.find_one_and_delete({}))
            if doc is None:
                break
            mine.append(doc["_id"])
        with lock:
            removed.extend(mine)
        cl.close()

    run_workers(WORKERS, worker)
    assert sorted(removed) == list(range(docs)), "a doc was double-claimed or lost"
    assert client["app"]["pool"].count_documents({}) == 0
    client.close()


def test_findandmodify_upsert_post_image_is_its_own_write(server):
    # new:true + upsert on a fresh key returns the upserted doc itself, not a
    # racy re-read.
    client = make_client(server)
    coll = client["app"]["c"]
    doc = coll.find_one_and_update(
        {"_id": "fresh"},
        {"$inc": {"n": 5}},
        upsert=True,
        return_document=pymongo.ReturnDocument.AFTER,
    )
    assert doc == {"_id": "fresh", "n": 5}
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
        got = coll0.count_documents({"x": bucket})
        if got != want:
            raise AssertionError(_index_scan_diff(coll0, bucket, got, want))
    client.close()


def _index_scan_diff(coll, bucket: int, got: int, want: int) -> str:
    """Explain an index/scan disagreement by naming the documents involved.

    This race is rare and has never reproduced locally (it needs real CI
    contention), so a bare ``assert 39 == 40`` throws away the only evidence
    the occurrence will ever produce. Diff the index against the collection and
    report exactly which ``_id``s the index lost (or invented), plus the plan
    that served the count — enough to tell "the build missed a concurrently
    inserted doc" apart from "the inserter wrote a wrong/duplicate key".

    Best-effort: any failure to gather detail is appended rather than raised, so
    the diagnostic can never mask or replace the real assertion.
    """
    lines = [f"bucket {bucket}: count_documents({{'x': {bucket}}}) == {got}, expected {want}"]
    try:
        via_index = {d["_id"] for d in coll.find({"x": bucket}, hint=[("x", 1)])}
        via_scan = {d["_id"] for d in coll.find({}) if d.get("x") == bucket}
        missing = sorted(via_scan - via_index)
        extra = sorted(via_index - via_scan)
        lines.append(f"  docs the INDEX is missing : {missing}")
        lines.append(f"  docs ONLY in the index    : {extra}")
        lines.append(f"  index={len(via_index)} scan={len(via_scan)}")
    except Exception as exc:  # noqa: BLE001 - diagnostics must never mask the failure
        lines.append(f"  (index/scan diff unavailable: {exc!r})")
    try:
        plan = coll.database.command(
            "explain", {"count": coll.name, "query": {"x": bucket}}, verbosity="queryPlanner"
        )
        lines.append(f"  winningPlan: {plan.get('queryPlanner', {}).get('winningPlan')}")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  (explain unavailable: {exc!r})")
    try:
        lines.append(f"  indexes: {[ix['name'] for ix in coll.list_indexes()]}")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  (list_indexes unavailable: {exc!r})")
    return "\n".join(lines)


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


def test_change_stream_sees_every_concurrent_insert(server):
    writer_n, docs_per = 4, 25
    client = make_client(server)
    coll0 = client["csdb"]["c"]
    coll0.insert_one({"_id": "seed"})  # create the collection

    cs = coll0.watch(max_await_time_ms=200)
    try:

        def writer(i: int) -> None:
            cl = make_client(server)
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


def test_db_change_stream_exactly_once_across_collections(server):
    """Cross-collection writers + a database-wide watch: exactly-once, no loss.

    This is the oplog minted-vs-committed visibility shape: writers on
    DIFFERENT collections don't share a collection lock, so one writer's
    oplog seq can be minted while another writer commits a later seq. The
    watch's position must never advance past the in-flight mint — if it
    does, that entry is lost (live AND on resume) when its transaction
    commits. Same-collection writers can't hit this (the collection lock
    serializes mint and commit), which is why the single-collection test
    above stays green even with the bug present.
    """
    writer_n, docs_per = 6, 40
    client = make_client(server)
    db = client["csxdb"]
    for i in range(writer_n):
        db[f"c{i}"].insert_one({"_id": "seed"})  # create the collections

    cs = db.watch(max_await_time_ms=200)
    try:

        def writer(i: int) -> None:
            cl = make_client(server)
            coll = cl["csxdb"][f"c{i}"]
            for k in range(docs_per):
                coll.insert_one({"_id": f"{i}-{k}", "who": i})
            cl.close()

        run_workers(writer_n, writer)

        want = writer_n * docs_per
        got: list[str] = []
        deadline = time.monotonic() + 30
        while len(set(got)) < want and time.monotonic() < deadline:
            ev = cs.try_next()
            if ev is None:
                continue
            if ev["operationType"] == "insert" and ev["documentKey"]["_id"] != "seed":
                got.append(ev["documentKey"]["_id"])
        assert len(got) == len(set(got)), "change stream delivered a duplicate event"
        assert len(set(got)) == want, (
            f"database change stream saw {len(set(got))} of {want} inserts — "
            "a cross-collection commit raced past an in-flight oplog mint"
        )
    finally:
        cs.close()
    client.close()
