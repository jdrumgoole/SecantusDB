"""Pymongo wrapper used by the admin app.

A thin facade over ``pymongo.MongoClient`` that:

* Caps server-selection so the UI fails fast when the target SecantusDB
  is unreachable, instead of hanging.
* Centralises error translation: pymongo's ``OperationFailure`` becomes
  a small ``MongoError`` we can catch in route handlers without leaking
  pymongo internals into templates.
* Exposes convenience helpers (``ping``, ``server_status``,
  ``build_info``, ``list_databases``, ``list_collections_with_stats``,
  ``paged_collection``) so route handlers don't reach into pymongo
  internals.
"""

from __future__ import annotations

import threading
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from pymongo import MongoClient
from pymongo.errors import OperationFailure, PyMongoError

from secantus.admin.pagination import (
    PageCursor,
    build_page_filter,
    make_next_cursor,
)

DEFAULT_PAGE_SIZE = 50


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

    # ---- databases + collections -----------------------------------------

    def list_databases(self) -> list[dict[str, Any]]:
        """Return the ``databases`` array from ``listDatabases``.

        Each entry has ``name`` plus best-effort ``sizeOnDisk`` / ``empty``
        as exposed by the target server.
        """
        out = self._run_admin("listDatabases")
        return [dict(d) for d in out.get("databases", []) or []]

    def list_collections_with_stats(self, db: str) -> list[dict[str, Any]]:
        """List collections in ``db``, attaching ``collStats`` per collection.

        Returns a list of ``{name, options, count, dataSize, indexSize,
        totalSize, indexSizes}`` rows. ``collStats`` failures degrade to
        zeros so a single weird collection doesn't kill the page.
        """
        try:
            client = self._get_client()
            db_obj = client[db]
            colls = list(db_obj.list_collections())
        except OperationFailure as exc:
            raise MongoError(str(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(str(exc)) from exc

        out: list[dict[str, Any]] = []
        for c in colls:
            name = c.get("name")
            if not isinstance(name, str):
                continue
            try:
                stats = dict(db_obj.command("collStats", name))
            except OperationFailure:
                stats = {}
            except PyMongoError:
                stats = {}
            out.append(
                {
                    "name": name,
                    "options": dict(c.get("options") or {}),
                    "count": int(stats.get("count", 0) or 0),
                    "dataSize": int(stats.get("size", 0) or 0),
                    "indexSize": int(stats.get("totalIndexSize", 0) or 0),
                    "totalSize": int(stats.get("totalSize", 0) or 0),
                    "indexSizes": dict(stats.get("indexSizes", {}) or {}),
                }
            )
        out.sort(key=lambda r: r["name"])
        return out

    def paged_collection(
        self,
        db: str,
        coll: str,
        *,
        filter_doc: Mapping[str, Any] | None = None,
        sort_dir: int = 1,
        cursor: PageCursor | None = None,
        page_size: int = DEFAULT_PAGE_SIZE,
    ) -> tuple[list[dict[str, Any]], str | None]:
        """Skip-ID-paginated find. Returns (docs, next_cursor_token | None).

        Asks for ``page_size + 1`` rows and trims the over-fetched one;
        if it came back the page isn't the last and we mint a cursor
        token for the next call.
        """
        if page_size <= 0:
            raise ValueError("page_size must be positive")
        find_filter = build_page_filter(filter_doc, cursor, sort_dir=sort_dir)
        try:
            coll_obj = self._get_client()[db][coll]
            rows = list(
                coll_obj.find(find_filter)
                .sort("_id", sort_dir)
                .limit(page_size + 1)
            )
        except OperationFailure as exc:
            raise MongoError(str(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(str(exc)) from exc

        next_token = make_next_cursor(rows, page_size)
        return rows[:page_size], next_token

    # ---- single-doc reads / writes ---------------------------------------

    def get_doc(self, db: str, coll: str, doc_id: Any) -> dict[str, Any] | None:
        try:
            return self._get_client()[db][coll].find_one({"_id": doc_id})
        except OperationFailure as exc:
            raise MongoError(str(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(str(exc)) from exc

    def replace_doc(
        self,
        db: str,
        coll: str,
        doc_id: Any,
        new_doc: dict[str, Any],
    ) -> int:
        """Replace the document at ``doc_id`` with ``new_doc``. Returns matched count.

        ``new_doc["_id"]`` is enforced to equal ``doc_id`` by the caller; we
        defensively assert it here too so an accidental ``_id`` rewrite
        never reaches the wire.
        """
        if "_id" in new_doc and new_doc["_id"] != doc_id:
            raise ValueError("_id is immutable; new document _id must match url")
        try:
            res = self._get_client()[db][coll].replace_one({"_id": doc_id}, new_doc)
        except OperationFailure as exc:
            raise MongoError(str(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(str(exc)) from exc
        return int(res.matched_count)

    def delete_doc(self, db: str, coll: str, doc_id: Any) -> int:
        """Delete the document at ``doc_id``. Returns deleted count (0 or 1)."""
        try:
            res = self._get_client()[db][coll].delete_one({"_id": doc_id})
        except OperationFailure as exc:
            raise MongoError(str(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(str(exc)) from exc
        return int(res.deleted_count)


__all__ = [
    "MongoFacade",
    "MongoError",
    "HealthResult",
    "DEFAULT_PAGE_SIZE",
]
