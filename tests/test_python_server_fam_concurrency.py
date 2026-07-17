"""findAndModify atomicity on the Python server under contention.

Pins the ``return_post_images`` fix: ``findAndModify {new: true}`` must return
the post-image of *its own* write, captured while the statement holds the
storage lock. Before the fix the handler re-``find``-ed the doc after the
update, so a concurrent findAndModify could land in between and two clients
were handed the same "new" document — duplicate tickets from the exact
pattern findAndModify exists to serve (measured: 8 duplicate tickets in 400
across 8 threads).

The Rust server still has this race (its findAndModify composes separate
storage calls) — see the xfail in ``test_rust_server_concurrency.py`` and
tasks/backlog.md.
"""

from __future__ import annotations

import threading

import pytest

pymongo = pytest.importorskip("pymongo")

from secantus.server import SecantusDBServer  # noqa: E402

WORKERS = 8


@pytest.fixture
def server(tmp_path):
    srv = SecantusDBServer(host="127.0.0.1", port=0, storage_path=str(tmp_path / "wt"))
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


def make_client(srv):
    host, port = srv.address
    return pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)


def test_findandmodify_tickets_are_unique_and_complete(server):
    per_worker = 25
    client = make_client(server)
    client["app"]["t"].insert_one({"_id": "counter", "n": 0})
    tickets: list[int] = []
    lock = threading.Lock()
    barrier = threading.Barrier(WORKERS)
    failures: list[BaseException] = []

    def worker(i: int) -> None:
        try:
            cl = make_client(server)
            coll = cl["app"]["t"]
            got: list[int] = []
            barrier.wait()
            for _ in range(per_worker):
                doc = coll.find_one_and_update(
                    {"_id": "counter"},
                    {"$inc": {"n": 1}},
                    return_document=pymongo.ReturnDocument.AFTER,
                )
                got.append(doc["n"])
            with lock:
                tickets.extend(got)
            cl.close()
        except BaseException as exc:  # noqa: BLE001 — surface into pytest
            failures.append(exc)

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(WORKERS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    if failures:
        raise failures[0]

    total = WORKERS * per_worker
    assert sorted(tickets) == list(range(1, total + 1)), "tickets were lost or duplicated"
    assert client["app"]["t"].find_one({"_id": "counter"})["n"] == total
    client.close()


def test_findandmodify_upsert_post_image_is_its_own_write(server):
    # new:true + upsert on a fresh key returns the upserted doc itself, not a
    # racy re-read.
    client = make_client(server)
    coll = client["app"]["t"]
    doc = coll.find_one_and_update(
        {"_id": "fresh"},
        {"$inc": {"n": 5}},
        upsert=True,
        return_document=pymongo.ReturnDocument.AFTER,
    )
    assert doc == {"_id": "fresh", "n": 5}
    client.close()
