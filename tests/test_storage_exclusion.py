"""WiredTiger enforces single-connection-per-directory exclusion.

The backlog flagged ``renameCollection`` as "atomic per the storage
RLock, but no protection against concurrent writers across
worktrees / processes." That second concern turns out to be moot:
WiredTiger itself takes an exclusive lock on the data directory
(``WiredTiger.lock`` in the home) at open time. Any second
:class:`Storage` open on the same path — same process or different
process, same thread or different thread — fails with
``WiredTigerError: Resource busy``.

That's a stronger guarantee than the storage-layer ``RLock``
provided on its own. The ``RLock`` covers concurrent writers from
multiple threads within a single ``Storage`` instance;
``WiredTiger.lock`` covers everything else by refusing to let a
second instance exist at all. ``rename_collection`` runs under the
``RLock``, so within-process it's atomic; across processes it's
unreachable.
"""

from __future__ import annotations

import pytest

from secantus.storage import Storage


def test_second_storage_open_on_same_path_fails(tmp_path) -> None:
    """A second ``Storage`` on the same on-disk path is rejected by
    WiredTiger with a ``Resource busy``-class error. The first
    instance keeps working — the exclusion is one-way, not a deadlock.
    """
    path = str(tmp_path / "wt_home")
    s1 = Storage(path=path)
    try:
        # First instance is fully usable.
        s1.insert("db", "coll", [{"_id": 1, "v": 10}])
        assert len(s1.find_matching("db", "coll", {})) == 1

        # Second open on the same path must fail.
        with pytest.raises(Exception) as exc_info:
            Storage(path=path)
        # WT's ``__conn_single`` raises a platform-translated ``WT_ERROR``;
        # the visible message differs per OS — macOS / Linux say
        # ``"Resource busy"``, Windows says ``"Resource device"`` — both
        # surface the same underlying second-open rejection. Match on
        # ``"resource"`` (common to every platform's translation) rather
        # than picking one side and breaking the matrix.
        assert "resource" in str(exc_info.value).lower()

        # The first instance is unaffected — still readable and writable.
        s1.insert("db", "coll", [{"_id": 2, "v": 20}])
        assert len(s1.find_matching("db", "coll", {})) == 2
    finally:
        s1.close()


def test_rename_collection_safe_after_close_and_reopen(tmp_path) -> None:
    """Closing the first instance releases the WT lock; a fresh
    ``Storage`` then opens cleanly and sees the renamed namespace.
    Pins the rename-then-reopen path that backup / restore workflows
    rely on."""
    path = str(tmp_path / "wt_home")
    s1 = Storage(path=path)
    try:
        s1.insert("appdb", "src", [{"_id": 1, "v": "kept"}])
        ok, err = s1.rename_collection("appdb", "src", "appdb", "dst")
        assert ok and err is None
    finally:
        s1.close()

    # Reopen — the rename survived the close + reopen round-trip.
    s2 = Storage(path=path)
    try:
        assert s2.find_matching("appdb", "src", {}) == []
        rows = s2.find_matching("appdb", "dst", {})
        assert [r["v"] for r in rows] == ["kept"]
    finally:
        s2.close()
