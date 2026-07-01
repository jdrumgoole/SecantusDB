"""Backup + restore via mongodump / mongorestore subprocesses.

The admin UI's backup slice runs the official Mongo tools as
subprocesses against the target SecantusDB. SecantusDB speaks the
mongo wire protocol faithfully, so the same tools that back up real
``mongod`` work unchanged here.

* ``preflight()`` — confirms ``mongodump`` and ``mongorestore`` are on
  PATH. Returns ``(ok, mongodump_path, mongorestore_path, error)``.
* ``run_mongodump(uri, root)`` — dumps to a fresh
  ``<root>/<UTC-stamp>/`` directory, returns ``BackupResult``.
* ``run_mongorestore(uri, dump_dir)`` — restores from ``dump_dir``.
* ``list_backups(root)`` — sorted list of past dumps under ``root``.

Subprocess invocation goes through an injectable ``runner`` callable
so tests don't need the real binaries on PATH.
"""

from __future__ import annotations

import datetime as _dt
import shutil
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

DEFAULT_BACKUP_ROOT = Path.home() / ".secantus" / "backups"


@dataclass(frozen=True)
class PreflightResult:
    ok: bool
    mongodump: str | None
    mongorestore: str | None
    error: str | None = None


@dataclass(frozen=True)
class BackupResult:
    ok: bool
    path: Path
    stdout: str
    stderr: str
    returncode: int


@dataclass(frozen=True)
class BackupEntry:
    name: str
    path: Path
    size_bytes: int
    created_at: float


# Type alias for the injectable subprocess runner. Default uses
# ``subprocess.run`` with ``capture_output=True``.
SubprocessRunner = Callable[[list[str]], subprocess.CompletedProcess]


def _default_runner(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def preflight(
    *,
    which: Callable[[str], str | None] = shutil.which,
) -> PreflightResult:
    """Look up ``mongodump`` and ``mongorestore`` on PATH.

    The ``which`` injector lets tests pretend the tools are present /
    absent without touching the real PATH.
    """
    dump_path = which("mongodump")
    restore_path = which("mongorestore")
    if dump_path is None or restore_path is None:
        missing = []
        if dump_path is None:
            missing.append("mongodump")
        if restore_path is None:
            missing.append("mongorestore")
        return PreflightResult(
            ok=False,
            mongodump=dump_path,
            mongorestore=restore_path,
            error=(
                "Mongo tools not on PATH: missing "
                f"{', '.join(missing)}. Install the official mongo-tools "
                "package (Homebrew: 'brew install mongodb/brew/mongodb-database-tools')."
            ),
        )
    return PreflightResult(ok=True, mongodump=dump_path, mongorestore=restore_path)


def _stamp(now: _dt.datetime | None = None) -> str:
    when = now or _dt.datetime.now(_dt.timezone.utc)
    return when.strftime("%Y%m%dT%H%M%SZ")


def _redact_password(text: str, uri: str) -> str:
    """Mask the password from a credentialed URI in captured tool output.

    ``mongodump`` / ``mongorestore`` echo the connection string
    (including the embedded password) into stderr on a connection
    failure. That output is captured into ``BackupResult`` and rendered
    in the admin UI, so a plaintext password could otherwise surface
    there. The URI must still reach the subprocess in the clear (it's the
    live credential), but nothing it prints back should carry the secret.
    """
    if not text:
        return text
    from urllib.parse import urlsplit

    try:
        password = urlsplit(uri).password
    except ValueError:
        password = None
    if password:
        text = text.replace(password, "***")
    return text


def run_mongodump(
    *,
    uri: str,
    root: Path | str = DEFAULT_BACKUP_ROOT,
    runner: SubprocessRunner = _default_runner,
    which: Callable[[str], str | None] = shutil.which,
    now: _dt.datetime | None = None,
) -> BackupResult:
    """Run ``mongodump --uri ... --out <root>/<stamp>/``."""
    pre = preflight(which=which)
    if not pre.ok or pre.mongodump is None:
        return BackupResult(
            ok=False,
            path=Path(root),
            stdout="",
            stderr=pre.error or "mongodump not on PATH",
            returncode=127,
        )
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    out_dir = root / _stamp(now)
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [pre.mongodump, "--uri", uri, "--out", str(out_dir)]
    proc = runner(cmd)
    return BackupResult(
        ok=proc.returncode == 0,
        path=out_dir,
        stdout=_redact_password(proc.stdout or "", uri),
        stderr=_redact_password(proc.stderr or "", uri),
        returncode=int(proc.returncode),
    )


def run_mongorestore(
    *,
    uri: str,
    dump_dir: Path | str,
    runner: SubprocessRunner = _default_runner,
    which: Callable[[str], str | None] = shutil.which,
) -> BackupResult:
    """Run ``mongorestore --uri ... <dump_dir>``."""
    pre = preflight(which=which)
    if not pre.ok or pre.mongorestore is None:
        return BackupResult(
            ok=False,
            path=Path(dump_dir),
            stdout="",
            stderr=pre.error or "mongorestore not on PATH",
            returncode=127,
        )
    dump_dir = Path(dump_dir)
    if not dump_dir.exists():
        return BackupResult(
            ok=False,
            path=dump_dir,
            stdout="",
            stderr=f"backup directory does not exist: {dump_dir}",
            returncode=2,
        )
    cmd = [pre.mongorestore, "--uri", uri, str(dump_dir)]
    proc = runner(cmd)
    return BackupResult(
        ok=proc.returncode == 0,
        path=dump_dir,
        stdout=_redact_password(proc.stdout or "", uri),
        stderr=_redact_password(proc.stderr or "", uri),
        returncode=int(proc.returncode),
    )


def _du(path: Path) -> int:
    """Recursive byte size — small backups, no need for stat caching."""
    if path.is_file():
        return path.stat().st_size
    total = 0
    for child in path.rglob("*"):
        if child.is_file():
            total += child.stat().st_size
    return total


def list_backups(root: Path | str = DEFAULT_BACKUP_ROOT) -> list[BackupEntry]:
    """Return existing backups under ``root``, newest-first.

    Includes both mongodump-style directories and native checkpoint
    archive files (``*.tar.gz`` produced by
    ``secantusAdmin.backupArchive``) so both kinds show up in the
    admin UI's "Existing backups" list with their respective restore
    actions.
    """
    root = Path(root)
    if not root.exists() or not root.is_dir():
        return []
    out: list[BackupEntry] = []
    for child in root.iterdir():
        try:
            stat = child.stat()
        except OSError:
            continue
        if child.is_dir():
            size = _du(child)
        elif child.is_file() and child.name.endswith(".tar.gz"):
            size = stat.st_size
        else:
            continue
        out.append(
            BackupEntry(
                name=child.name,
                path=child,
                size_bytes=size,
                created_at=stat.st_mtime,
            )
        )
    out.sort(key=lambda e: e.created_at, reverse=True)
    return out


__all__ = [
    "PreflightResult",
    "BackupResult",
    "BackupEntry",
    "DEFAULT_BACKUP_ROOT",
    "preflight",
    "run_mongodump",
    "run_mongorestore",
    "list_backups",
]
