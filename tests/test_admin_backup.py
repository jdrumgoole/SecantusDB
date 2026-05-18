"""Backup helpers — preflight + dump + restore + list."""

from __future__ import annotations

import datetime as dt
import subprocess

from secantus.admin.backup import (
    list_backups,
    preflight,
    run_mongodump,
    run_mongorestore,
)


def _fake_which_present(name: str) -> str | None:
    if name in ("mongodump", "mongorestore"):
        return f"/usr/local/bin/{name}"
    return None


def _fake_which_missing(_name: str) -> str | None:
    return None


def _ok_runner(captured: list[list[str]]):
    def runner(cmd: list[str]) -> subprocess.CompletedProcess:
        captured.append(cmd)
        return subprocess.CompletedProcess(args=cmd, returncode=0, stdout="ok", stderr="")

    return runner


def _fail_runner() -> subprocess.CompletedProcess:
    def runner(cmd: list[str]) -> subprocess.CompletedProcess:
        return subprocess.CompletedProcess(args=cmd, returncode=1, stdout="", stderr="boom")

    return runner


# ---- preflight -------------------------------------------------------------


def test_preflight_ok_when_both_tools_present() -> None:
    pre = preflight(which=_fake_which_present)
    assert pre.ok is True
    assert pre.mongodump == "/usr/local/bin/mongodump"
    assert pre.mongorestore == "/usr/local/bin/mongorestore"


def test_preflight_reports_missing_tools() -> None:
    pre = preflight(which=_fake_which_missing)
    assert pre.ok is False
    assert "mongodump" in (pre.error or "")
    assert "mongorestore" in (pre.error or "")


# ---- run_mongodump --------------------------------------------------------


def test_run_mongodump_invokes_with_uri_and_out(tmp_path) -> None:
    captured: list[list[str]] = []
    out = run_mongodump(
        uri="mongodb://localhost:1234",
        root=tmp_path,
        runner=_ok_runner(captured),
        which=_fake_which_present,
        now=dt.datetime(2026, 5, 10, 12, 34, 56, tzinfo=dt.timezone.utc),
    )
    assert out.ok is True
    assert out.path == tmp_path / "20260510T123456Z"
    assert out.path.is_dir()
    assert captured == [
        [
            "/usr/local/bin/mongodump",
            "--uri",
            "mongodb://localhost:1234",
            "--out",
            str(out.path),
        ]
    ]


def test_run_mongodump_propagates_failure(tmp_path) -> None:
    out = run_mongodump(
        uri="mongodb://localhost:1234",
        root=tmp_path,
        runner=_fail_runner(),
        which=_fake_which_present,
    )
    assert out.ok is False
    assert out.returncode == 1
    assert "boom" in out.stderr


def test_run_mongodump_aborts_when_tools_missing(tmp_path) -> None:
    captured: list[list[str]] = []
    out = run_mongodump(
        uri="mongodb://x",
        root=tmp_path,
        runner=_ok_runner(captured),  # should never be called
        which=_fake_which_missing,
    )
    assert out.ok is False
    assert "mongodump" in out.stderr
    # The runner shouldn't fire when preflight failed.
    assert captured == []


# ---- run_mongorestore ------------------------------------------------------


def test_run_mongorestore_invokes_with_uri_and_dir(tmp_path) -> None:
    captured: list[list[str]] = []
    dump_dir = tmp_path / "dump"
    dump_dir.mkdir()
    out = run_mongorestore(
        uri="mongodb://x",
        dump_dir=dump_dir,
        runner=_ok_runner(captured),
        which=_fake_which_present,
    )
    assert out.ok is True
    assert captured == [["/usr/local/bin/mongorestore", "--uri", "mongodb://x", str(dump_dir)]]


def test_run_mongorestore_rejects_missing_dir(tmp_path) -> None:
    captured: list[list[str]] = []
    out = run_mongorestore(
        uri="mongodb://x",
        dump_dir=tmp_path / "no-such",
        runner=_ok_runner(captured),
        which=_fake_which_present,
    )
    assert out.ok is False
    assert "does not exist" in out.stderr
    assert captured == []


# ---- list_backups ----------------------------------------------------------


def test_list_backups_empty_returns_empty(tmp_path) -> None:
    assert list_backups(tmp_path) == []
    assert list_backups(tmp_path / "no-such") == []


def test_list_backups_sorts_newest_first(tmp_path) -> None:
    a = tmp_path / "20260101T000000Z"
    b = tmp_path / "20260201T000000Z"
    a.mkdir()
    b.mkdir()
    # Make ``b`` newer than ``a`` regardless of mkdir order.
    import os as _os

    _os.utime(a, (0, 1.0))
    _os.utime(b, (0, 2.0))
    entries = list_backups(tmp_path)
    assert [e.name for e in entries] == [b.name, a.name]


def test_list_backups_reports_size(tmp_path) -> None:
    backup = tmp_path / "20260101T000000Z"
    backup.mkdir()
    payload = b"x" * 1024
    (backup / "data.bson").write_bytes(payload)
    [entry] = list_backups(tmp_path)
    assert entry.size_bytes == 1024
    assert entry.path == backup


def test_list_backups_includes_native_tar_gz_archives(tmp_path) -> None:
    """``archive-<stamp>.tar.gz`` from the native checkpoint backup
    path shows up alongside mongodump directories."""
    dump = tmp_path / "20260101T000000Z"
    dump.mkdir()
    (dump / "data.bson").write_bytes(b"x")
    archive = tmp_path / "archive-20260101T000000.tar.gz"
    archive.write_bytes(b"\x1f\x8b" + b"x" * 1024)  # arbitrary gz-shaped blob
    names = sorted(e.name for e in list_backups(tmp_path))
    assert names == sorted([archive.name, dump.name])


def test_list_backups_ignores_unrelated_files(tmp_path) -> None:
    """Files that aren't ``.tar.gz`` and aren't directories are skipped."""
    (tmp_path / "readme.txt").write_text("ignore me")
    (tmp_path / "scratch.bson").write_bytes(b"...")
    assert list_backups(tmp_path) == []
