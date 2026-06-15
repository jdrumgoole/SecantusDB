"""``listDatabases`` populates ``sizeOnDisk`` and ``empty`` per database.

Before this slice, every entry in the ``listDatabases`` response
carried a placeholder ``sizeOnDisk: 0`` and ``empty: False``.
mongo-rust-driver's ``test::client::list_databases`` (and any
admin tool that displays db sizes) asserted ``size_on_disk > 0``
on populated databases and saw 0, which it treated as a fatal
correctness gap. The fix computes the size per database as the
sum of bson-encoded doc bytes across its collections — the same
accounting ``collStats`` / ``dbStats`` already use — and derives
``empty`` from the size.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


def test_size_on_disk_nonzero_for_populated_db(client) -> None:
    """A db with data has ``sizeOnDisk > 0`` and ``empty: false``."""
    client["sizing_db"]["coll"].insert_many([{"_id": i, "payload": "x" * 100} for i in range(5)])

    resp = client["admin"].command("listDatabases")
    entry = next(d for d in resp["databases"] if d["name"] == "sizing_db")
    assert entry["sizeOnDisk"] > 0
    assert entry["empty"] is False


def test_total_size_sums_per_db_sizes(client) -> None:
    """``totalSize`` is the sum of every db's ``sizeOnDisk``."""
    client["db_a"]["coll"].insert_one({"x": 1})
    client["db_b"]["coll"].insert_one({"x": 2})

    resp = client["admin"].command("listDatabases")
    per_db = {d["name"]: d["sizeOnDisk"] for d in resp["databases"]}
    assert resp["totalSize"] == sum(per_db.values())


def test_name_only_skips_size_field(client) -> None:
    """``nameOnly: true`` returns only ``{name}`` entries — no
    ``sizeOnDisk`` walk. Lets drivers do cheap name listings without
    paying for the per-collection iteration."""
    client["a"]["coll"].insert_one({"_id": 1})

    resp = client["admin"].command("listDatabases", nameOnly=True)
    for entry in resp["databases"]:
        assert set(entry.keys()) == {"name"}


def test_filter_applies_to_full_descriptors(client) -> None:
    """``filter`` runs against the ``{name, sizeOnDisk, empty}``
    descriptors — drivers use it to scope listings (e.g.
    ``{empty: false}``)."""
    client["filt"]["coll"].insert_one({"x": 1})

    resp = client["admin"].command("listDatabases", filter={"name": "filt"})
    assert [d["name"] for d in resp["databases"]] == ["filt"]
    assert resp["databases"][0]["sizeOnDisk"] > 0
