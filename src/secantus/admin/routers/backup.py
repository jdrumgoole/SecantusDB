"""Backup + restore page.

Lists existing backups under ``~/.secantus/backups/`` and offers three
actions:

* "Run mongodump" — POST /backup/dump runs ``mongodump`` against the
  target URI and writes to a fresh stamped directory. Slower than the
  native path below but produces a portable BSON dump that any mongod
  can ingest.
* "Run native checkpoint backup" — POST /backup/archive runs the
  ``secantusAdmin.backupArchive`` wire command. Forces a WT
  checkpoint then tars the storage directory into a single
  ``.tar.gz``. Faster + atomic vs mongodump; SecantusDB-specific
  format (restore is "extract + start a new SecantusDB pointing at
  it").
* "Restore" per BSON backup — POST /backup/restore with a name
  parameter runs ``mongorestore`` from the named directory.

All three wait for the operation to finish and re-render the page
with the captured stdout / stderr / size in a flash panel.
"""

from __future__ import annotations

import datetime as _dt
import time as _time
from pathlib import Path

from fastapi import APIRouter, Form, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin import backup as backup_lib
from secantus.admin.client import MongoError

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _backup_root(request: Request) -> Path:
    return getattr(request.app.state, "backup_root", backup_lib.DEFAULT_BACKUP_ROOT)


def _humanize_bytes(n: int | float | None) -> str:
    if not n:
        return "0 B"
    n = float(n)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024 or unit == "TB":
            return f"{int(n)} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


def _render(
    request: Request,
    *,
    flash: dict[str, str] | None = None,
    last_result: backup_lib.BackupResult | None = None,
) -> HTMLResponse:
    pre = backup_lib.preflight()
    backups = backup_lib.list_backups(_backup_root(request))
    rows = [
        {
            "name": b.name,
            "size": _humanize_bytes(b.size_bytes),
            "created_at": b.created_at,
        }
        for b in backups
    ]
    templates = _templates(request)
    return templates.TemplateResponse(
        request,
        "pages/backup.html",
        {
            "title": "Backup",
            "active": "backup",
            "preflight_ok": pre.ok,
            "preflight_error": pre.error,
            "backup_root": str(_backup_root(request)),
            "rows": rows,
            "flash": flash,
            "last_result": last_result,
        },
    )


@router.get("/backup", response_class=HTMLResponse)
def backup_page(request: Request) -> HTMLResponse:
    return _render(request)


@router.post("/backup/dump", response_class=HTMLResponse)
def post_dump(request: Request) -> HTMLResponse:
    result = backup_lib.run_mongodump(
        uri=request.app.state.mongo_uri,
        root=_backup_root(request),
    )
    flash = {
        "kind": "ok" if result.ok else "err",
        "msg": (
            f"mongodump → {result.path.name}"
            if result.ok
            else f"mongodump failed (exit {result.returncode})"
        ),
    }
    return _render(request, flash=flash, last_result=result)


@router.post("/backup/archive", response_class=HTMLResponse)
def post_archive(request: Request) -> HTMLResponse:
    """Run the native ``secantusAdmin.backupArchive`` wire command.

    Writes a ``.tar.gz`` under the same ``backup_root`` mongodump uses
    so the user sees both kinds of backup side-by-side. The output
    path stamps with a timestamp so successive runs don't overwrite.
    """
    root = _backup_root(request)
    root.mkdir(parents=True, exist_ok=True)
    stamp = _dt.datetime.now().strftime("%Y%m%dT%H%M%S")
    archive = root / f"archive-{stamp}.tar.gz"
    started = _time.monotonic()
    try:
        result = request.app.state.mongo.backup_archive(str(archive))
    except MongoError as exc:
        flash = {"kind": "err", "msg": f"backupArchive failed: {exc}"}
        return _render(request, flash=flash)
    elapsed_ms = int((_time.monotonic() - started) * 1000)
    size_pretty = _humanize_bytes(result.get("sizeBytes", 0))
    flash = {
        "kind": "ok",
        "msg": (f"backupArchive → {archive.name} ({size_pretty} in {elapsed_ms} ms)"),
    }
    return _render(request, flash=flash)


@router.post("/backup/restore", response_class=HTMLResponse)
def post_restore(
    request: Request,
    name: str = Form(...),
) -> HTMLResponse:
    # The form value is a directory name under backup_root, never a
    # raw path — guard against directory-traversal so a craft input
    # can't point mongorestore at, say, /etc.
    if "/" in name or ".." in name or not name.strip():
        return _render(
            request,
            flash={"kind": "err", "msg": f"invalid backup name: {name!r}"},
        )
    dump_dir = _backup_root(request) / name
    result = backup_lib.run_mongorestore(
        uri=request.app.state.mongo_uri,
        dump_dir=dump_dir,
    )
    flash = {
        "kind": "ok" if result.ok else "err",
        "msg": (
            f"mongorestore from {name}"
            if result.ok
            else f"mongorestore failed (exit {result.returncode})"
        ),
    }
    return _render(request, flash=flash, last_result=result)


@router.post("/backup/restore-archive", response_class=HTMLResponse)
def post_restore_archive(
    request: Request,
    name: str = Form(...),
    target_dir: str = Form(...),
) -> HTMLResponse:
    """Run the native ``secantusAdmin.restoreArchive`` wire command.

    ``name`` is the archive filename under ``backup_root`` (typically
    ``archive-<stamp>.tar.gz``). ``target_dir`` is the absolute path
    the server should extract into — the operator then starts a new
    SecantusDB process pointed at that dir to switch over.
    """
    if "/" in name or ".." in name or not name.strip():
        return _render(
            request,
            flash={"kind": "err", "msg": f"invalid archive name: {name!r}"},
        )
    target = target_dir.strip()
    if not target or ".." in target:
        return _render(
            request,
            flash={
                "kind": "err",
                "msg": f"invalid target directory: {target_dir!r}",
            },
        )
    archive = _backup_root(request) / name
    try:
        result = request.app.state.mongo.restore_archive(str(archive), target)
    except MongoError as exc:
        flash = {"kind": "err", "msg": f"restoreArchive failed: {exc}"}
        return _render(request, flash=flash)
    flash = {
        "kind": "ok",
        "msg": (
            f"restoreArchive → {result['fileCount']} file(s) extracted to "
            f"{result['targetDir']}. Restart SecantusDB with "
            f"--storage-path {result['targetDir']} to switch."
        ),
    }
    return _render(request, flash=flash)
