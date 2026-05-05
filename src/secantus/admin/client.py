"""Pymongo wrapper used by the admin app.

A thin facade over ``pymongo.MongoClient`` that:

* Caps server-selection so the UI fails fast when the target SecantusDB
  is unreachable, instead of hanging.
* Centralises error translation: pymongo's ``OperationFailure`` becomes
  a small ``MongoError`` we can catch in route handlers without leaking
  pymongo internals into templates.
* Exposes a couple of convenience helpers (``ping``, ``server_status``,
  ``build_info``) so the dashboard isn't littered with raw
  ``client.admin.command(...)`` calls.

Pagination helpers (skip-ID cursor handling) land in slice 2 alongside
the collection viewer that needs them.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Any

from pymongo import MongoClient
from pymongo.errors import OperationFailure, PyMongoError


class MongoError(Exception):
    """Translated pymongo failure surfaced by ``MongoFacade``."""

    def __init__(self, msg: str, *, code: int | None = None) -> None:
        super().__init__(msg)
        self.code = code


@dataclass
class HealthResult:
    ok: bool
    detail: str


class MongoFacade:
    """Thread-safe wrapper around a pymongo client.

    The underlying ``MongoClient`` is created lazily on first use so the
    FastAPI app can be constructed in tests without immediately needing
    a reachable mongo. Subsequent accesses share the same client; the
    facade can be ``close()``-d to release it.
    """

    def __init__(self, mongo_uri: str, *, server_selection_timeout_ms: int = 2000) -> None:
        self._uri = mongo_uri
        self._timeout_ms = server_selection_timeout_ms
        self._client: MongoClient | None = None
        self._lock = threading.Lock()

    def _get_client(self) -> MongoClient:
        with self._lock:
            if self._client is None:
                self._client = MongoClient(
                    self._uri,
                    serverSelectionTimeoutMS=self._timeout_ms,
                )
            return self._client

    def close(self) -> None:
        with self._lock:
            if self._client is not None:
                self._client.close()
                self._client = None

    # ---- convenience helpers ----------------------------------------------

    def ping(self) -> HealthResult:
        try:
            self._get_client().admin.command("ping")
            return HealthResult(ok=True, detail="ok")
        except PyMongoError as exc:
            return HealthResult(ok=False, detail=str(exc))

    def server_status(self) -> dict[str, Any]:
        return self._run_admin("serverStatus")

    def build_info(self) -> dict[str, Any]:
        return self._run_admin("buildInfo")

    def _run_admin(self, name: str, /, **kwargs: Any) -> dict[str, Any]:
        try:
            return dict(self._get_client().admin.command(name, **kwargs))
        except OperationFailure as exc:
            raise MongoError(str(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(str(exc)) from exc


__all__ = ["MongoFacade", "MongoError", "HealthResult"]
