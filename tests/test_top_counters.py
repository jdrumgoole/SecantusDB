"""``top`` reports real per-namespace operation counters.

Every ``{time, count}`` used to be hard-zero, so ``mongotop`` rendered an idle
server no matter the load. The section mapping here was probed against real
mongod 8.3.4 rather than assumed -- and the assumptions were wrong in four
places: ``aggregate``, ``count``, ``distinct`` and ``findAndModify`` all land in
``commands``, not in ``queries``/``update``. mongod's ``queries`` section is
essentially just ``find``.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def client(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


def _top(client, ns):
    return client.admin.command("top")["totals"].get(ns, {})


def _counts(entry):
    return {k: v["count"] for k, v in entry.items()}


def test_counters_are_no_longer_all_zero(client) -> None:
    db = client["topdb"]
    db.c.insert_many([{"x": i} for i in range(5)])
    list(db.c.find({}))
    entry = _top(client, "topdb.c")
    assert entry, "namespace missing from top"
    assert any(v["count"] for v in entry.values()), f"all counters still zero: {entry}"


def test_sections_match_mongod_classification(client) -> None:
    db = client["topdb2"]
    db.c.insert_many([{"x": i} for i in range(3)])  # one command, not three
    list(db.c.find({}))
    db.c.update_one({"x": 1}, {"$set": {"y": 1}})
    db.c.delete_one({"x": 2})

    got = _counts(_top(client, "topdb2.c"))
    assert got["insert"] == 1, got
    assert got["queries"] == 1, got
    assert got["update"] == 1, got
    assert got["remove"] == 1, got
    # total == readLock + writeLock for this workload, as on mongod.
    assert got["total"] == 4, got
    assert got["readLock"] == 1, got
    assert got["writeLock"] == 3, got


@pytest.mark.parametrize(
    "label,op,section,lock",
    [
        ("aggregate", lambda db: list(db.c.aggregate([{"$match": {}}])), "commands", "readLock"),
        ("count", lambda db: db.c.count_documents({}), "commands", "readLock"),
        ("distinct", lambda db: db.c.distinct("x"), "commands", "readLock"),
        ("listIndexes", lambda db: list(db.c.list_indexes()), "commands", "readLock"),
        ("createIndexes", lambda db: db.c.create_index("x"), "commands", "writeLock"),
        (
            "findAndModify",
            lambda db: db.c.find_one_and_update({"x": 0}, {"$set": {"z": 1}}),
            "commands",
            "writeLock",
        ),
    ],
)
def test_command_bucket_and_lock_kind(client, label, op, section, lock) -> None:
    """These six are exactly where the naive mapping was wrong."""
    db = client[f"topdb_{label.lower()}"]
    db.c.insert_one({"x": 0})
    ns = f"topdb_{label.lower()}.c"
    before = _counts(_top(client, ns))
    op(db)
    after = _counts(_top(client, ns))
    assert after[section] == before[section] + 1, f"{label}: {section} did not move"
    assert after[lock] == before[lock] + 1, f"{label}: {lock} did not move"


def test_insert_counts_commands_not_documents(client) -> None:
    db = client["topdb3"]
    db.c.insert_many([{"x": i} for i in range(50)])
    assert _counts(_top(client, "topdb3.c"))["insert"] == 1


def test_time_is_recorded_in_microseconds(client) -> None:
    db = client["topdb4"]
    db.c.insert_many([{"x": i} for i in range(200)])
    entry = _top(client, "topdb4.c")
    assert entry["insert"]["time"] > 0, entry
    assert entry["total"]["time"] >= entry["insert"]["time"]


def test_drop_resets_the_namespace(client) -> None:
    """Probed on mongod 8.3.4: a dropped collection restarts from zero."""
    db = client["topdb5"]
    db.c.insert_many([{"x": i} for i in range(5)])
    list(db.c.find({}))
    assert _counts(_top(client, "topdb5.c"))["total"] >= 2
    db.c.drop()
    db.c.insert_one({"x": 1})
    assert _counts(_top(client, "topdb5.c"))["insert"] == 1


def test_non_namespaced_commands_are_not_attributed(client) -> None:
    """``ping``/``hello`` never appear in top, as on mongod."""
    db = client["topdb6"]
    db.c.insert_one({"x": 1})
    before = _counts(_top(client, "topdb6.c"))
    for _ in range(5):
        client.admin.command("ping")
        client.admin.command("hello")
    assert _counts(_top(client, "topdb6.c")) == before


def test_shape_is_unchanged(client) -> None:
    """mongo-tools' decoder requires the note key and all nine sections."""
    db = client["topdb7"]
    db.c.insert_one({"x": 1})
    totals = client.admin.command("top")["totals"]
    assert totals["note"] == "all times in microseconds"
    entry = totals["topdb7.c"]
    assert set(entry) == {
        "total",
        "readLock",
        "writeLock",
        "queries",
        "getmore",
        "insert",
        "update",
        "remove",
        "commands",
    }
    for section in entry.values():
        assert set(section) == {"time", "count"}


def test_top_still_refuses_non_admin_db(client) -> None:
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        client["notadmin"].command("top")
    assert exc.value.code == 13
