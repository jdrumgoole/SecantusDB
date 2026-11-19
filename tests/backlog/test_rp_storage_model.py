"""Gap tests for the PostgreSQL server's side of the move to mongod's storage
model (`tasks/mongod-storage-model-plan.md`, decisions 3 and 7).

`secantusd-pg` shares `secantus-storage` with `secantusd-mdb`, so a store it
writes must be one mongod 8.2.11 opens too, with each table a collection
(table `widgets` in database `postgres` is the collection `postgres.widgets`,
its PRIMARY KEY the `_id`, as the SQL-to-MongoDB hand-off already does
through `secantusd-mdb`).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.backlog.test_rm_storage_model import _mongod_on, _stop_mongod, _wait_primary


def _pg_store(home: Path) -> None:
    from tests.test_rust_pgserver_slice import _Server

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE widgets (id int PRIMARY KEY, name text, qty int)")
        cur.execute("INSERT INTO widgets VALUES (1,'bolt',40),(2,'nut',75)")


def _count(path: Path, prefix: str) -> int:
    return sum(1 for p in path.iterdir() if p.name.startswith(prefix) and p.suffix == ".wt")


@pytest.mark.backlog("RP-1074")
def test_a_table_is_its_own_collection_table(tmp_path: Path) -> None:
    """A table is stored as mongod stores a collection: its own
    `collection-<UUID>.wt` table, and an `index-<UUID>.wt` table for its
    PRIMARY KEY (the `_id_` index)."""
    from tests.test_rust_pgserver_slice import _Server

    base = tmp_path / "base"
    with _Server(base) as server, server.connect() as conn:
        conn.cursor().execute("SELECT 1")
    store = tmp_path / "store"
    _pg_store(store)
    grew = (
        _count(store, "collection-") - _count(base, "collection-"),
        _count(store, "index-") - _count(base, "index-"),
    )
    assert grew == (1, 1)


@pytest.mark.backlog("RP-1074")
def test_mongod_reads_a_table_the_sql_server_wrote(tmp_path: Path) -> None:
    """mongod 8.2.11 started with `--replSet` on a store `secantusd-pg`
    wrote sees the table as a collection, with the documents a MongoDB
    client of `secantusd-mdb` sees today (no `__sys_*` fields), and
    `validate({full: true})` passes."""
    store = tmp_path / "store"
    _pg_store(store)
    proc, client = _mongod_on(store)
    try:
        assert client is not None, "mongod refused the store (see its log beside the store)"
        host, port = client.address or ("127.0.0.1", 0)
        _wait_primary(client, f"{host}:{port}")
        coll = client["postgres"]["widgets"]
        docs = sorted(coll.find({}), key=lambda d: d["_id"])
        assert docs == [
            {"_id": 1, "name": "bolt", "qty": 40},
            {"_id": 2, "name": "nut", "qty": 75},
        ]
        result = client["postgres"].command("validate", "widgets", full=True)
        assert (result["valid"], result["nrecords"]) == (True, 2)
    finally:
        _stop_mongod(proc, client)
