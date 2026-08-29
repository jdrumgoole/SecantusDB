"""A cloned WiredTiger home must be indistinguishable from a freshly-created one.

``wt_template`` speeds the per-test fixture floor up by copying a prebuilt,
cleanly-closed WiredTiger home instead of asking WiredTiger to create ~12 tables
per test (~137 ms of the ~235 ms ``Storage()`` cost — see
``tasks/rust-test-harness-investigation.md``). That is only a legitimate
optimisation if a cloned home behaves *identically* to a created one, so these
tests pin the equivalence rather than merely asserting the clone opens.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from secantus.storage import Storage
from wt_template import build_template, clone_template


def _exercise(store: Storage) -> dict[str, Any]:
    """Drive a spread of storage behaviour and return everything observable.

    Deliberately spans the tables the template pre-creates: the collection
    registry, the documents shards, the ``_id`` index, the secondary-index
    catalog + entries, and the oplog.
    """
    store.create_collection("shopdb", "orders")
    inserted, errors = store.insert(
        "shopdb",
        "orders",
        [{"_id": i, "sku": f"sku-{i % 3}", "qty": i * 2} for i in range(6)],
    )
    store.create_index("shopdb", "orders", "sku_1", {"sku": 1})

    by_sku = store.find_matching("shopdb", "orders", {"sku": "sku-1"})
    ranged = store.find_matching("shopdb", "orders", {"qty": {"$gte": 4}}, sort={"qty": -1})
    updated = store.update_matching(
        "shopdb", "orders", {"sku": "sku-0"}, {"$set": {"flagged": True}}, multi=True
    )
    deleted = store.delete_matching("shopdb", "orders", {"_id": 5})

    return {
        "inserted": inserted,
        "errors": errors,
        "by_sku": by_sku,
        "ranged": ranged,
        "updated": updated,
        "deleted": deleted,
        "count": store.count_matching("shopdb", "orders", {}),
        "collections": sorted(store.list_collections("shopdb")),
        "indexes": sorted(i.get("name", "") for i in store.list_indexes("shopdb", "orders")),
        "final": store.find_matching("shopdb", "orders", {}, sort={"_id": 1}),
        "explain": store.explain_plan("shopdb", "orders", {"sku": "sku-1"}),
    }


@pytest.fixture
def template(tmp_path: Path) -> Path:
    home = tmp_path / "template"
    build_template(str(home))
    return home


def test_clone_is_functionally_identical_to_a_fresh_home(template: Path, tmp_path: Path) -> None:
    fresh_dir = tmp_path / "fresh"
    fresh_dir.mkdir()
    fresh = Storage(str(fresh_dir))
    try:
        expected = _exercise(fresh)
    finally:
        fresh.close()

    cloned_dir = tmp_path / "cloned"
    clone_template(str(template), str(cloned_dir))
    cloned = Storage(str(cloned_dir))
    try:
        actual = _exercise(cloned)
    finally:
        cloned.close()

    assert actual == expected


def test_clone_starts_empty(template: Path, tmp_path: Path) -> None:
    dest = tmp_path / "empty"
    clone_template(str(template), str(dest))
    store = Storage(str(dest))
    try:
        assert store.list_collections("shopdb") == []
        assert store.list_collections("admin") == []
    finally:
        store.close()


def test_clones_are_isolated_from_each_other_and_the_template(
    template: Path, tmp_path: Path
) -> None:
    first = tmp_path / "a"
    second = tmp_path / "b"
    clone_template(str(template), str(first))
    clone_template(str(template), str(second))

    a = Storage(str(first))
    try:
        a.create_collection("db1", "things")
        a.insert("db1", "things", [{"_id": 1, "v": "from-a"}])
    finally:
        a.close()

    b = Storage(str(second))
    try:
        assert b.list_collections("db1") == []
    finally:
        b.close()

    # A third clone taken AFTER the first was written must still be pristine —
    # i.e. writing through a clone did not reach back into the template.
    third = tmp_path / "c"
    clone_template(str(template), str(third))
    c = Storage(str(third))
    try:
        assert c.list_collections("db1") == []
    finally:
        c.close()


def test_cloned_home_survives_close_and_reopen(template: Path, tmp_path: Path) -> None:
    dest = tmp_path / "reopen"
    clone_template(str(template), str(dest))

    store = Storage(str(dest), durable=True)
    try:
        store.create_collection("db1", "things")
        store.insert("db1", "things", [{"_id": 7, "v": "persisted"}])
    finally:
        store.close()

    again = Storage(str(dest), durable=True)
    try:
        assert again.find_matching("db1", "things", {"_id": 7}) == [{"_id": 7, "v": "persisted"}]
    finally:
        again.close()
