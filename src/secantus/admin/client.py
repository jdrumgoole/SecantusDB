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


def friendly_error(exc: BaseException) -> str:
    """Render a pymongo exception as a one-line human-friendly string.

    PyMongo's ``ServerSelectionTimeoutError.__str__`` appends the full
    topology description (multi-line, hundreds of chars of internal
    state). That's noise for an end-user "couldn't connect" message —
    it leaks pymongo internals into a popup that should say "the host
    is down."

    Strategy: chop everything after ``", Timeout:"`` (where the topology
    block starts), drop ``"(configured timeouts: ...)"`` boilerplate,
    keep the first line. Returns the exception class name as a fallback
    when the message is empty.
    """
    import re

    s = str(exc)
    # ServerSelectionTimeoutError appends the topology description after
    # ", Timeout:" — drop everything from that marker onward.
    if ", Timeout:" in s:
        s = s.split(", Timeout:", 1)[0]
    # OperationFailure appends ", full error: { ... }" with the raw
    # server reply dict — same noise.
    if ", full error:" in s:
        s = s.split(", full error:", 1)[0]
    s = s.split("\n", 1)[0].strip()
    s = re.sub(r"\s*\(configured timeouts:[^)]*\)", "", s)
    return s or type(exc).__name__


def display_uri(uri: str) -> str:
    """Strip the password from a MongoDB URI for display.

    Keeps the username (it's useful context — "who am I connecting as")
    and drops the password and the trailing query string. Returns the
    input unchanged when no userinfo is present.

    >>> display_uri("mongodb://127.0.0.1:27017/")
    'mongodb://127.0.0.1:27017'
    >>> display_uri("mongodb://alice:s3cret@host:27017/?authSource=admin")
    'mongodb://alice@host:27017'
    """
    from urllib.parse import urlsplit, urlunsplit

    try:
        parts = urlsplit(uri)
    except ValueError:
        return uri
    if not parts.netloc:
        return uri
    username = parts.username
    host = parts.hostname or ""
    port = parts.port
    netloc = f"{username}@{host}" if username else host
    if port:
        netloc = f"{netloc}:{port}"
    # Drop the path's trailing slash and the query string — pymongo URIs
    # often carry ``/?authSource=admin`` boilerplate that's noise in a
    # status badge.
    path = parts.path.rstrip("/")
    return urlunsplit((parts.scheme, netloc, path, "", ""))


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
            return HealthResult(ok=False, detail=friendly_error(exc))

    def server_status(self) -> dict[str, Any]:
        return self._run_admin("serverStatus")

    def build_info(self) -> dict[str, Any]:
        return self._run_admin("buildInfo")

    def _run_admin(self, name: str, /, **kwargs: Any) -> dict[str, Any]:
        try:
            return dict(self._get_client().admin.command(name, **kwargs))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

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
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

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
            rows = list(coll_obj.find(find_filter).sort("_id", sort_dir).limit(page_size + 1))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

        next_token = make_next_cursor(rows, page_size)
        return rows[:page_size], next_token

    # ---- single-doc reads / writes ---------------------------------------

    def get_doc(self, db: str, coll: str, doc_id: Any) -> dict[str, Any] | None:
        try:
            return self._get_client()[db][coll].find_one({"_id": doc_id})
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

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
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc
        return int(res.matched_count)

    def delete_doc(self, db: str, coll: str, doc_id: Any) -> int:
        """Delete the document at ``doc_id``. Returns deleted count (0 or 1)."""
        try:
            res = self._get_client()[db][coll].delete_one({"_id": doc_id})
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc
        return int(res.deleted_count)

    # ---- indexes ---------------------------------------------------------

    def list_indexes(self, db: str, coll: str) -> list[dict[str, Any]]:
        """Return the index list for ``db.coll`` as plain dicts.

        Empty when the collection doesn't exist (so the page can render
        an empty table cleanly instead of erroring).
        """
        try:
            return [dict(ix) for ix in self._get_client()[db][coll].list_indexes()]
        except OperationFailure as exc:
            if exc.code in (26,):  # NamespaceNotFound
                return []
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def create_index(
        self,
        db: str,
        coll: str,
        key: list[tuple[str, int | str]],
        *,
        name: str | None = None,
        unique: bool = False,
        sparse: bool = False,
        partial_filter_expression: Mapping[str, Any] | None = None,
        expire_after_seconds: int | None = None,
    ) -> str:
        """Create an index. Returns the resulting index name."""
        kwargs: dict[str, Any] = {"unique": unique, "sparse": sparse}
        if name:
            kwargs["name"] = name
        if partial_filter_expression is not None:
            kwargs["partialFilterExpression"] = dict(partial_filter_expression)
        if expire_after_seconds is not None:
            kwargs["expireAfterSeconds"] = int(expire_after_seconds)
        try:
            return self._get_client()[db][coll].create_index(key, **kwargs)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def drop_index(self, db: str, coll: str, name: str) -> None:
        if name == "_id_":
            raise MongoError("cannot drop _id_ index")
        try:
            self._get_client()[db][coll].drop_index(name)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- explain ---------------------------------------------------------

    def explain_find(
        self,
        db: str,
        coll: str,
        *,
        filter_doc: Mapping[str, Any] | None = None,
        sort: Mapping[str, Any] | None = None,
        hint: Any = None,
    ) -> dict[str, Any]:
        """Run ``explain`` for a ``find`` and return the response."""
        find_cmd: dict[str, Any] = {"find": coll, "filter": dict(filter_doc or {})}
        if sort:
            find_cmd["sort"] = dict(sort)
        if hint is not None:
            find_cmd["hint"] = hint
        try:
            return dict(self._get_client()[db].command("explain", find_cmd))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- users ----------------------------------------------------------

    def list_users(self, db: str) -> list[dict[str, Any]]:
        """Return all users in ``db``. Each entry has ``user``, ``db``, ``roles``."""
        try:
            out = self._get_client()[db].command("usersInfo", 1)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc
        return [dict(u) for u in out.get("users", []) or []]

    def get_user(self, db: str, username: str) -> dict[str, Any] | None:
        try:
            out = self._get_client()[db].command("usersInfo", username)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc
        users = out.get("users", []) or []
        return dict(users[0]) if users else None

    def create_user(
        self,
        db: str,
        username: str,
        password: str,
        roles: list[dict[str, str] | str],
    ) -> None:
        try:
            self._get_client()[db].command("createUser", username, pwd=password, roles=list(roles))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def update_user_password(self, db: str, username: str, password: str) -> None:
        try:
            self._get_client()[db].command("updateUser", username, pwd=password)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def drop_user(self, db: str, username: str) -> None:
        try:
            self._get_client()[db].command("dropUser", username)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def grant_roles(
        self,
        db: str,
        username: str,
        roles: list[dict[str, str] | str],
    ) -> None:
        if not roles:
            return
        try:
            self._get_client()[db].command("grantRolesToUser", username, roles=list(roles))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def revoke_roles(
        self,
        db: str,
        username: str,
        roles: list[dict[str, str] | str],
    ) -> None:
        if not roles:
            return
        try:
            self._get_client()[db].command("revokeRolesFromUser", username, roles=list(roles))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- ad-hoc query console -------------------------------------------

    def run_find(
        self,
        db: str,
        coll: str,
        *,
        filter_doc: Mapping[str, Any] | None = None,
        sort: Mapping[str, Any] | None = None,
        projection: Mapping[str, Any] | None = None,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        """Run a console-driven ``find``. Returns up to ``limit`` docs.

        ``limit`` is clamped to ``[1, 200]`` so a runaway query can't OOM
        the admin process.
        """
        n = max(1, min(int(limit), 200))
        try:
            coll_obj = self._get_client()[db][coll]
            cursor = coll_obj.find(
                dict(filter_doc or {}), projection=dict(projection) if projection else None
            ).limit(n)
            if sort:
                cursor = cursor.sort(list(dict(sort).items()))
            return [dict(d) for d in cursor]
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def run_aggregate(
        self,
        db: str,
        coll: str,
        pipeline: list[Any],
        *,
        limit: int = 200,
    ) -> list[dict[str, Any]]:
        """Run a console-driven aggregation. Returns up to ``limit`` docs."""
        n = max(1, min(int(limit), 1000))
        try:
            coll_obj = self._get_client()[db][coll]
            out: list[dict[str, Any]] = []
            for d in coll_obj.aggregate(list(pipeline)):
                out.append(dict(d))
                if len(out) >= n:
                    break
            return out
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def insert_many(
        self,
        db: str,
        coll: str,
        docs: list[Mapping[str, Any]],
    ) -> dict[str, Any]:
        """Insert one or more documents into ``db.coll``.

        Returns ``{"inserted_count": N, "inserted_ids": [...]}`` so the
        UI can display per-doc ``_id`` values (auto-assigned ones in
        particular). The pymongo driver mutates the input dicts to add
        ``_id`` when missing, which the caller passes through to the
        response.
        """
        if not docs:
            raise MongoError("at least one document is required")
        try:
            coll_obj = self._get_client()[db][coll]
            result = coll_obj.insert_many([dict(d) for d in docs], ordered=True)
            return {
                "inserted_count": len(result.inserted_ids),
                "inserted_ids": list(result.inserted_ids),
            }
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def run_command(self, db: str, command: Mapping[str, Any]) -> dict[str, Any]:
        """Run an arbitrary command against ``db``. Returns the response doc."""
        try:
            return dict(self._get_client()[db].command(dict(command)))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- profiler -------------------------------------------------------

    def get_profile(self, db: str) -> dict[str, Any]:
        """Return the current profile state for ``db``."""
        try:
            out = self._get_client()[db].command("profile", -1)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc
        return {
            "level": int(out.get("was", 0) or 0),
            "slowms": int(out.get("slowms", 100) or 100),
            "sampleRate": float(out.get("sampleRate", 1.0) or 1.0),
        }

    def set_profile(
        self,
        db: str,
        *,
        level: int,
        slowms: int = 100,
        sample_rate: float = 1.0,
    ) -> None:
        """Update profile state for ``db``."""
        try:
            self._get_client()[db].command(
                "profile", int(level), slowms=int(slowms), sampleRate=float(sample_rate)
            )
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- currentOp + cursor management ----------------------------------

    def current_op(self) -> list[dict[str, Any]]:
        """Return the ``inprog`` array from ``currentOp`` (connections + cursors)."""
        out = self._run_admin("currentOp")
        return [dict(e) for e in out.get("inprog", []) or []]

    def kill_cursor(self, ns: str, cursor_id: int) -> dict[str, Any]:
        """Issue ``killCursors`` for ``cursor_id`` against the namespace ``ns``.

        ``ns`` is ``db.coll``. The wire command is per-collection — for
        cursors that aren't naturally tied to a collection (cluster-wide
        change streams, e.g. ``ns == ""``), the caller passes ``"admin.$cmd"``
        and the command is rejected with a clear error.
        """
        db, _, coll = ns.partition(".")
        if not db or not coll:
            raise MongoError(f"cannot kill cursor in namespace {ns!r}")
        try:
            return dict(
                self._get_client()[db].command("killCursors", coll, cursors=[int(cursor_id)])
            )
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def kill_connection(self, conn_id: int) -> dict[str, Any]:
        """Issue ``killOp`` against the connection's ``opid``.

        SecantusDB maps ``opid`` to ``conn_id`` one-to-one (each
        connection has one in-flight op), so the value the admin UI
        shows in the ``conn_id`` column on /connections is what the
        ``killOp`` command takes as ``op``.
        """
        try:
            return dict(self._get_client().admin.command("killOp", op=int(conn_id)))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def backup_archive(self, output_path: str) -> dict[str, Any]:
        """Issue ``secantusAdmin.backupArchive`` against the target.

        Forces a WT checkpoint then tars the storage directory into
        ``output_path``. The target server must be SecantusDB — real
        ``mongod`` rejects unknown commands.
        """
        try:
            return dict(
                self._get_client().admin.command(
                    "secantusAdmin.backupArchive", outputPath=str(output_path)
                )
            )
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def restore_archive(
        self,
        archive_path: str,
        target_dir: str,
        *,
        allow_existing: bool = False,
    ) -> dict[str, Any]:
        """Issue ``secantusAdmin.restoreArchive`` against the target.

        Server-side extracts the archive into ``target_dir``. The
        caller then points a *new* SecantusDB process at that dir to
        switch to the restored data — the currently-running server's
        storage is not modified.
        """
        try:
            return dict(
                self._get_client().admin.command(
                    "secantusAdmin.restoreArchive",
                    archivePath=str(archive_path),
                    targetDir=str(target_dir),
                    allowExisting=bool(allow_existing),
                )
            )
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- maintenance ---------------------------------------------------

    def fsync(self) -> dict[str, Any]:
        """Force a WiredTiger checkpoint via the ``fsync`` command."""
        return self._run_admin("fsync")

    def prune_oplog(self) -> int:
        """Drop oplog rows past the retention window. Returns docs pruned."""
        out = self._run_admin("secantusAdmin.pruneOplog")
        return int(out.get("pruned", 0) or 0)

    def prune_ttl(self) -> int:
        """Run TTL pruning against every collection. Returns docs pruned."""
        out = self._run_admin("secantusAdmin.pruneTtl")
        return int(out.get("pruned", 0) or 0)

    def drop_database(self, db: str) -> None:
        """Drop ``db`` entirely. Mongod-shape ``dropDatabase`` command."""
        try:
            self._get_client()[db].command("dropDatabase")
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def drop_collection(self, db: str, coll: str) -> None:
        """Drop a single collection."""
        try:
            self._get_client()[db].drop_collection(coll)
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    # ---- schema sampler / logs / geo ------------------------------------

    def sample_collection(self, db: str, coll: str, *, size: int = 100) -> list[dict[str, Any]]:
        """Return up to ``size`` random docs via ``$sample``.

        ``$sample`` is the right primitive for schema inference: it gives
        an unbiased look at the collection without paging through the
        whole thing. ``size`` is clamped to ``[1, 1000]``.
        """
        n = max(1, min(int(size), 1000))
        try:
            cursor = self._get_client()[db][coll].aggregate([{"$sample": {"size": n}}])
            return [dict(d) for d in cursor]
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def get_log(self, name: str = "global") -> dict[str, Any]:
        """Return the response of ``getLog``: ``{log: [...], totalLinesWritten}``."""
        try:
            return dict(self._get_client().admin.command("getLog", name))
        except OperationFailure as exc:
            raise MongoError(friendly_error(exc), code=exc.code) from exc
        except PyMongoError as exc:
            raise MongoError(friendly_error(exc)) from exc

    def geo_indexes(self, db: str, coll: str) -> list[dict[str, Any]]:
        """Return only indexes that look like ``2dsphere`` / ``2d``."""
        ixs = self.list_indexes(db, coll)
        out: list[dict[str, Any]] = []
        for ix in ixs:
            key = ix.get("key") or {}
            for v in key.values():
                if v in ("2dsphere", "2d"):
                    out.append(ix)
                    break
        return out


__all__ = [
    "MongoFacade",
    "MongoError",
    "HealthResult",
    "DEFAULT_PAGE_SIZE",
    "display_uri",
    "friendly_error",
]
