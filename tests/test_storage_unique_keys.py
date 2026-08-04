"""Unique indexes are enforced by WiredTiger, not by a snapshot read.

The index-entries table keys by ``sortkey + RecordId``, so two documents
sharing an indexed value produce DIFFERENT keys and never collide. Uniqueness
therefore had to be a probe read through the caller's own snapshot, which left
two holes it could not see:

* a value another transaction committed AFTER your snapshot was taken;
* two transactions inserting the same value at once, neither yet committed.

``table:secantus_unique_keys`` keys by the indexed value itself, so the storage
engine enforces it: a duplicate is WiredTiger's own WT_DUPLICATE_KEY, and a
race is a write-write conflict rather than two silent successes.
"""

from __future__ import annotations

import threading

import pytest

from secantus.storage import Storage


@pytest.fixture()
def store(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _values(store: Storage) -> list:
    return sorted(d["v"] for d in store.find_matching("db", "c", {}))


class TestBasicEnforcement:
    def test_duplicate_is_rejected(self, store):
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        assert store.insert("db", "c", [{"_id": 1, "v": 10}])[0] == 1
        inserted, errors = store.insert("db", "c", [{"_id": 2, "v": 10}])
        assert inserted == 0
        assert errors and errors[0]["code"] == 11000
        assert errors[0]["keyValue"] == {"v": 10}
        assert _values(store) == [10]

    def test_distinct_values_insert(self, store):
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        store.insert("db", "c", [{"_id": 1, "v": 10}])
        assert store.insert("db", "c", [{"_id": 2, "v": 11}])[0] == 1

    def test_rejected_insert_leaves_nothing_behind(self, store):
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        store.insert("db", "c", [{"_id": 1, "v": 10}])
        store.insert("db", "c", [{"_id": 2, "v": 10}])
        assert [d["_id"] for d in store.find_matching("db", "c", {})] == [1]


class TestKeysAreReleased:
    """A claim must be given back, or a value could never be reused."""

    def test_delete_then_reinsert(self, store):
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        store.insert("db", "c", [{"_id": 1, "v": 10}])
        store.delete_matching("db", "c", {"v": 10})
        assert store.insert("db", "c", [{"_id": 2, "v": 10}])[0] == 1

    def test_update_frees_the_old_value(self, store):
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        store.insert("db", "c", [{"_id": 1, "v": 10}])
        store.update_matching("db", "c", {"_id": 1}, {"$set": {"v": 20}})
        assert store.insert("db", "c", [{"_id": 2, "v": 10}])[0] == 1
        assert _values(store) == [10, 20]

    def test_update_onto_a_taken_value_is_refused(self, store):
        from secantus.storage import IndexConflict

        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        store.insert("db", "c", [{"_id": 1, "v": 10}, {"_id": 2, "v": 20}])
        with pytest.raises(IndexConflict):
            store.update_matching("db", "c", {"_id": 2}, {"$set": {"v": 10}})
        assert _values(store) == [10, 20]


class TestBackfill:
    def test_index_created_over_existing_rows_claims_them(self, store):
        store.insert("db", "c", [{"_id": 1, "v": 10}, {"_id": 2, "v": 11}])
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        assert store.insert("db", "c", [{"_id": 3, "v": 10}])[0] == 0
        assert store.insert("db", "c", [{"_id": 4, "v": 12}])[0] == 1


class TestTheTwoHolesThatMotivatedThis:
    def test_value_committed_after_our_snapshot(self, store):
        """The probe reads the caller's snapshot, so it cannot see this. The
        storage engine can."""
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        txn = store.begin_user_transaction()
        with store.use_user_transaction(txn):
            store.find_matching("db", "c", {})  # pin the snapshot

        other = threading.Thread(target=lambda: store.insert("db", "c", [{"_id": 1, "v": 42}]))
        other.start()
        other.join()

        # WiredTiger refuses the key outright: inside a transaction that is a
        # write conflict, which is what mongod raises there too.
        with pytest.raises(Exception, match="(?i)conflict|WT_ROLLBACK|duplicate"):
            with store.use_user_transaction(txn):
                store.insert("db", "c", [{"_id": 2, "v": 42}])
            store.commit_user_transaction(txn)
        store.abort_user_transaction(txn)
        assert _values(store) == [42], "the duplicate must not have landed"

    def test_simultaneous_inserts_of_one_value(self, store):
        store.create_index("db", "c", "uv", {"v": 1}, {"unique": True})
        workers = 8
        barrier = threading.Barrier(workers)
        wins = [0] * workers

        def run(i: int) -> None:
            barrier.wait()
            try:
                wins[i] = store.insert("db", "c", [{"_id": i, "v": 99}])[0]
            except Exception:
                wins[i] = 0

        threads = [threading.Thread(target=run, args=(i,)) for i in range(workers)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert sum(wins) == 1, "exactly one insert may win"
        assert _values(store) == [99]
