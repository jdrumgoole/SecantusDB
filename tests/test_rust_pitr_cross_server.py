"""Cross-server point-in-time recovery (Phase R, R6a).

Retired 2026-11-20 (tasks/mongod-storage-model-plan.md, phase 4): the four
tests that rebuilt a stopped Rust server's data directory, a mark, a backup
archive and a capped collection's options with the deprecated Python
``oplog_replay`` restore tool -- ``test_python_restores_stopped_rust_data_dir``,
``test_python_restores_rust_data_to_a_mark``,
``test_python_restores_rust_backup_archive`` and
``test_rust_create_options_survive_restore``. The Rust servers write mongod's
layout, which the Python store does not read; compatibility with the Python
servers stopped being a constraint on 2026-10-16 (CLAUDE.md). What is lost: a
check that the Python tool rebuilds a Rust store. The Rust restore's own
tests (``tests/test_rust_binary_pitr.py``) cover a data directory, a
timestamp, an archive and a capped collection's options through
``secantusd-mdb restore``.

Gated on the WiredTiger-linking ``_secantus_server`` extension.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

_server = pytest.importorskip("_secantus_server")
# The Python server writes without timestamps, which a store whose data
# tables are not logged (secantusd-mdb's default since RM-210) refuses once a
# key has one: the hand-off is tested on a store created with them logged.
pymongo = pytest.importorskip("pymongo")

from secantus.storage import Storage  # noqa: E402


def _docs(path: Path, db: str, coll: str) -> list[dict[str, Any]]:
    s = Storage(str(path), enable_oplog=True)
    try:
        return sorted(s.find_matching(db, coll, {}), key=lambda d: d["_id"])
    finally:
        s.close()


def _rust_client(srv: Any):
    host, port = srv.address
    return pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)


def test_rust_backup_archive_requires_output_path(tmp_path: Path) -> None:
    """``backupArchive`` without ``outputPath`` is a clean TypeMismatch error."""
    srv = _server.RustServer(str(tmp_path / "data"), 0, data_nonlogged=False)
    try:
        reply = _rust_client(srv)["admin"].command({"secantusAdmin.backupArchive": 1}, check=False)
        assert reply["ok"] == 0.0
        assert reply["code"] == 14
    finally:
        srv.stop()
