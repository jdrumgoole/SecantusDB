"""Backup + restore page.

Lists existing backups under ``~/.secantus/backups/`` and offers two
actions:

* "Run mongodump" — POST /backup/dump runs ``mongodump`` against the
  target URI and writes to a fresh stamped directory.
* "Restore" per backup — POST /backup/restore with a name parameter
  runs ``mongorestore`` from the named directory.

Both wait for the subprocess to finish (no streaming progress in v1)
and re-render the page with the captured stdout / stderr in a flash
panel below the actions.

WT-checkpoint → tar backup is intentionally not exposed here yet — see
``tasks/backlog.md``.
"""

from __future__ import annotations

from pathlib import Path

from fastapi import APIRouter, Form, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from secantus.admin import backup as backup_lib

router = APIRouter()


def _templates(request: Request) -> Jinja2Templates:
    return Jinja2Templates(directory=request.app.state.templates_dir)


def _backup_root(request: Request) -> Path:
    return getattr(
        request.app.state, "backup_root", backup_lib.DEFAULT_BACKUP_ROOT
    )


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
