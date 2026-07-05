"""``Storage`` context-manager protocol (security review §I12).

``with Storage(path) as store:`` guarantees WiredTiger teardown on block exit
(threads joined, oplog meta persisted, connection closed) even if the body
raises, instead of relying on the embedder to remember ``close()``. ``close()``
is idempotent, so an explicit close inside the block is still safe.
"""

from __future__ import annotations

import pytest

from secantus.storage import Storage


def test_with_block_closes_storage(tmp_path):
    with Storage(str(tmp_path)) as store:
        assert isinstance(store, Storage)
        store.insert("db", "c", [{"_id": 1, "x": 10}])
        assert [d["x"] for d in store.find_matching("db", "c", {})] == [10]
    # Block exit closed the store.
    assert store._closed is True


def test_with_block_closes_on_exception(tmp_path):
    store_ref = None
    with pytest.raises(ValueError, match="boom"), Storage(str(tmp_path)) as store:
        store_ref = store
        raise ValueError("boom")
    # Closed despite the exception, and the exception propagated (not suppressed).
    assert store_ref is not None
    assert store_ref._closed is True


def test_explicit_close_inside_block_is_safe(tmp_path):
    # close() is idempotent, so calling it inside the block and again on exit
    # must not raise.
    with Storage(str(tmp_path)) as store:
        store.close()
        assert store._closed is True
    assert store._closed is True


def test_reopen_after_with_block(tmp_path):
    # Data written inside a with-block persists and is readable after reopen —
    # proves the block exit actually flushed/closed WiredTiger cleanly.
    with Storage(str(tmp_path)) as store:
        store.insert("db", "c", [{"_id": 1, "x": 42}])
    reopened = Storage(str(tmp_path))
    try:
        assert [d["x"] for d in reopened.find_matching("db", "c", {})] == [42]
    finally:
        reopened.close()
