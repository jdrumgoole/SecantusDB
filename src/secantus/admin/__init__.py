"""SecantusDB admin web UI.

Local FastAPI app served behind a pywebview window. Loopback-only,
gated by a per-launch token. Connects to a target SecantusDB (or any
MongoDB-wire-compatible server) over pymongo via the URI passed to
``--uri``.

Entry points:

* ``secantusdb-admin`` console script — argparse → ``cli.main``.
* ``python -m secantus.admin`` — equivalent.
* ``secantus.admin.app.create_app(mongo_uri=..., token=...)`` — the
  FastAPI app factory; tests construct it directly.
"""

from __future__ import annotations

__all__ = ["create_app"]


def create_app(
    *,
    mongo_uri: str,
    token: str,
    history_path=None,
    backup_root=None,
    embedded_storage=None,
):
    """Re-export ``app.create_app`` lazily to avoid pulling fastapi for tests
    that only import ``secantus.admin``."""
    from secantus.admin.app import create_app as _factory

    return _factory(
        mongo_uri=mongo_uri,
        token=token,
        history_path=history_path,
        backup_root=backup_root,
        embedded_storage=embedded_storage,
    )
