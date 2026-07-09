"""Side-channel restore: ``extract_backup_archive`` unpacks a backup
into a fresh dir that a new ``SecantusDBServer`` can point at.

Hot in-place restore (replace a live storage's WT home without
restarting) is intentionally not supported — connection threads
cache WT sessions lock-free, so swapping the connection under them
would race into native segfaults. Restore is therefore "extract +
start a fresh server pointing at the extracted dir," which is the
same shape ``tests/test_backup_restore.py`` already exercises and
matches how real ``mongod`` restore tooling works.
"""

from __future__ import annotations

import tarfile
from pathlib import Path
from typing import Any

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer
from secantus.storage import Storage, extract_backup_archive


def _take_backup(client: MongoClient, archive: Path) -> None:
    res = client.admin.command("secantusAdmin.backupArchive", outputPath=str(archive))
    assert res["ok"] == 1.0, res


def _make_archive(tmp_path: Path, contents: dict[str, Any] | None = None) -> Path:
    """Produce a real SecantusDB backup archive in ``tmp_path``."""
    src = tmp_path / "src"
    storage = Storage(str(src))
    try:
        storage.insert("appdb", "items", [{"_id": 1, "v": "snapshot"}])
        archive = tmp_path / "snap.tar.gz"
        storage.create_archive(str(archive))
    finally:
        storage.close()
    return archive


def test_extract_unpacks_into_fresh_target(tmp_path) -> None:
    archive = _make_archive(tmp_path)
    target = tmp_path / "restored"
    result = extract_backup_archive(str(archive), str(target))
    assert Path(result["targetDir"]).resolve() == target.resolve()
    assert result["fileCount"] > 0
    assert (target / "WiredTiger").is_file()
    # A new server points at the extracted dir and sees the snapshot.
    srv = SecantusDBServer(port=0, storage_path=str(target))
    srv.start()
    try:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            rows = list(client["appdb"]["items"].find())
            assert rows == [{"_id": 1, "v": "snapshot"}]
        finally:
            client.close()
    finally:
        srv.stop()


def test_extract_refuses_non_wt_archive(tmp_path) -> None:
    bogus = tmp_path / "bogus.tar.gz"
    (tmp_path / "readme.txt").write_text("not a backup")
    with tarfile.open(bogus, "w:gz") as t:
        t.add(tmp_path / "readme.txt", arcname="readme.txt")
    target = tmp_path / "restored"
    with pytest.raises(RuntimeError, match="not a SecantusDB backup"):
        extract_backup_archive(str(bogus), str(target))
    # Target dir was created (we create before validating contents) but
    # nothing got extracted into it.
    assert target.is_dir()
    assert not any(target.iterdir())


def test_extract_rejects_missing_archive(tmp_path) -> None:
    with pytest.raises(RuntimeError, match="not found"):
        extract_backup_archive(str(tmp_path / "missing.tar.gz"), str(tmp_path / "restored"))


def test_extract_rejects_non_empty_target(tmp_path) -> None:
    archive = _make_archive(tmp_path)
    target = tmp_path / "restored"
    target.mkdir()
    (target / "junk.txt").write_text("existing file")
    with pytest.raises(RuntimeError, match="not empty"):
        extract_backup_archive(str(archive), str(target))
    # Pre-existing file untouched.
    assert (target / "junk.txt").read_text() == "existing file"
    # ``allow_existing=True`` overlays.
    result = extract_backup_archive(str(archive), str(target), allow_existing=True)
    assert result["fileCount"] > 0
    assert (target / "WiredTiger").is_file()
    assert (target / "junk.txt").read_text() == "existing file"


def test_extract_rejects_target_that_is_a_file(tmp_path) -> None:
    archive = _make_archive(tmp_path)
    target = tmp_path / "restored.txt"
    target.write_text("I'm a file, not a dir")
    with pytest.raises(RuntimeError, match="not a directory"):
        extract_backup_archive(str(archive), str(target))


def test_wire_restore_archive_round_trip(tmp_path) -> None:
    """``secantusAdmin.restoreArchive`` extracts a backup over the wire.
    A fresh server pointed at the target dir sees the snapshot."""
    archive = tmp_path / "snap.tar.gz"
    target = tmp_path / "restored"
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "src"))
    srv.start()
    try:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            client["appdb"]["items"].insert_one({"_id": 7, "v": "wire-test"})
            client.admin.command("secantusAdmin.backupArchive", outputPath=str(archive))
            result = client.admin.command(
                "secantusAdmin.restoreArchive",
                archivePath=str(archive),
                targetDir=str(target),
            )
            assert result["ok"] == 1.0
            assert Path(result["targetDir"]).resolve() == target.resolve()
            assert result["fileCount"] > 0
        finally:
            client.close()
    finally:
        srv.stop()
    # Fresh server reads the restored docs.
    srv2 = SecantusDBServer(port=0, storage_path=str(target))
    srv2.start()
    try:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            rows = list(c2["appdb"]["items"].find())
            assert rows == [{"_id": 7, "v": "wire-test"}]
        finally:
            c2.close()
    finally:
        srv2.stop()


def test_cli_extracts_into_target(tmp_path, capsys) -> None:
    """``secantus-restore-archive --archive X --target-dir Y`` extracts."""
    from secantus.restore_cli import main as restore_main

    archive = _make_archive(tmp_path)
    target = tmp_path / "cli-restored"
    rc = restore_main(["--archive", str(archive), "--target-dir", str(target)])
    assert rc == 0
    out = capsys.readouterr().out
    assert "Extracted" in out
    assert str(target) in out
    assert "secantusdb --storage-path" in out
    assert (target / "WiredTiger").is_file()
    srv = SecantusDBServer(port=0, storage_path=str(target))
    srv.start()
    try:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            assert client["appdb"]["items"].count_documents({}) == 1
        finally:
            client.close()
    finally:
        srv.stop()


def test_cli_reports_errors(tmp_path, capsys) -> None:
    """Failure modes return rc=2 and print the error on stderr."""
    from secantus.restore_cli import main as restore_main

    rc = restore_main(
        [
            "--archive",
            str(tmp_path / "missing.tar.gz"),
            "--target-dir",
            str(tmp_path / "x"),
        ]
    )
    assert rc == 2
    err = capsys.readouterr().err
    assert "error:" in err
    assert "not found" in err


def test_wire_restore_archive_surfaces_validation_errors(tmp_path) -> None:
    """Bad inputs return ``ok: 0`` with a typed error code, not a crash."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "src"))
    srv.start()
    try:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            # Missing archivePath.
            from pymongo.errors import OperationFailure

            with pytest.raises(OperationFailure) as exc:
                client.admin.command(
                    "secantusAdmin.restoreArchive",
                    targetDir=str(tmp_path / "x"),
                )
            assert "archivePath" in str(exc.value)
            # Archive that doesn't exist.
            with pytest.raises(OperationFailure) as exc:
                client.admin.command(
                    "secantusAdmin.restoreArchive",
                    archivePath=str(tmp_path / "missing.tar.gz"),
                    targetDir=str(tmp_path / "x"),
                )
            assert "not found" in str(exc.value)
        finally:
            client.close()
    finally:
        srv.stop()
