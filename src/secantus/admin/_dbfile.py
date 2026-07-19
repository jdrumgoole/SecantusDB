"""Owner-only permissions for the admin console's on-disk sqlite state.

``~/.secantus/admin.db`` holds two tables that between them are as
sensitive as the admin token sitting next to them:

* ``connection_targets`` (:mod:`secantus.admin.targets`) stores target
  URIs **verbatim, including credentials**. That is deliberate — the
  "switch to this target" buttons have to be able to reconnect, and a
  scrubbed URI can't authenticate — but it means the file is a
  plaintext credential store.
* ``recent_queries`` (:mod:`secantus.admin.history`) stores console
  payloads, which routinely contain real document data.

``cli.py`` already chmods the token file to ``0600``; this applies the
same treatment to the database, which was previously left at whatever
the process umask produced (commonly ``0644`` — world-readable). The
directory is tightened to ``0700`` as well, because sqlite's ``-wal`` /
``-journal`` sidecars are created on demand and would otherwise land at
the default mode during a write.

Both operations are best-effort: filesystems without POSIX modes
(Windows, some network mounts) raise ``OSError``, and a permissions
failure must never stop the admin console from starting.
"""

from __future__ import annotations

import contextlib
import os
from pathlib import Path

DIR_MODE = 0o700
FILE_MODE = 0o600


def secure_sqlite_file(path: Path) -> None:
    """Restrict ``path`` (and its parent directory) to the owner.

    Safe to call repeatedly and safe to call before the file exists —
    the parent is tightened either way, and the file is chmod'ed once
    sqlite has created it.
    """
    with contextlib.suppress(OSError):
        os.chmod(path.parent, DIR_MODE)
    if path.exists():
        with contextlib.suppress(OSError):
            os.chmod(path, FILE_MODE)


__all__ = ["secure_sqlite_file", "DIR_MODE", "FILE_MODE"]
