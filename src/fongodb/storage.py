from __future__ import annotations

import sqlite3
import threading
from collections.abc import Iterable
from typing import Any

import bson

from fongodb.query import matches
from fongodb.update import apply_update

_SCHEMA = """
CREATE TABLE IF NOT EXISTS _fongodb_collections (
    db_name   TEXT NOT NULL,
    coll_name TEXT NOT NULL,
    options   BLOB,
    PRIMARY KEY (db_name, coll_name)
);

CREATE TABLE IF NOT EXISTS _fongodb_documents (
    db_name   TEXT NOT NULL,
    coll_name TEXT NOT NULL,
    id_key    BLOB NOT NULL,
    doc       BLOB NOT NULL,
    PRIMARY KEY (db_name, coll_name, id_key)
);
"""


class DuplicateKeyError(Exception):
    def __init__(self, doc_id: Any) -> None:
        super().__init__(f"duplicate _id: {doc_id!r}")
        self.doc_id = doc_id


def _id_key(doc_id: Any) -> bytes:
    return bson.encode({"_": doc_id})


class Storage:
    def __init__(self, path: str = ":memory:") -> None:
        self._conn = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self._conn.executescript(_SCHEMA)
        self._lock = threading.RLock()

    def close(self) -> None:
        with self._lock:
            self._conn.close()

    def _ensure_collection(self, db: str, coll: str) -> None:
        self._conn.execute(
            "INSERT OR IGNORE INTO _fongodb_collections(db_name, coll_name) VALUES (?, ?)",
            (db, coll),
        )

    def collection_exists(self, db: str, coll: str) -> bool:
        with self._lock:
            row = self._conn.execute(
                "SELECT 1 FROM _fongodb_collections WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            ).fetchone()
            return row is not None

    def create_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            cursor = self._conn.execute(
                "INSERT OR IGNORE INTO _fongodb_collections(db_name, coll_name) VALUES (?, ?)",
                (db, coll),
            )
            return cursor.rowcount > 0

    def insert(
        self, db: str, coll: str, docs: Iterable[dict[str, Any]], *, ordered: bool = True
    ) -> tuple[int, list[dict[str, Any]]]:
        inserted = 0
        errors: list[dict[str, Any]] = []
        with self._lock:
            self._ensure_collection(db, coll)
            for index, doc in enumerate(docs):
                if "_id" not in doc:
                    doc["_id"] = bson.ObjectId()
                key = _id_key(doc["_id"])
                blob = bson.encode(doc)
                try:
                    self._conn.execute(
                        "INSERT INTO _fongodb_documents(db_name, coll_name, id_key, doc) "
                        "VALUES (?, ?, ?, ?)",
                        (db, coll, key, blob),
                    )
                    inserted += 1
                except sqlite3.IntegrityError:
                    errors.append(
                        {
                            "index": index,
                            "code": 11000,
                            "errmsg": f"E11000 duplicate key error: _id {doc['_id']!r}",
                        }
                    )
                    if ordered:
                        break
        return inserted, errors

    def _all_docs(self, db: str, coll: str) -> list[dict[str, Any]]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT doc FROM _fongodb_documents WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            ).fetchall()
        return [bson.decode(blob) for (blob,) in rows]

    def find_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        skip: int = 0,
        limit: int = 0,
    ) -> list[dict[str, Any]]:
        filter = filter or {}
        out: list[dict[str, Any]] = []
        for doc in self._all_docs(db, coll):
            if matches(doc, filter):
                out.append(doc)
        if skip:
            out = out[skip:]
        if limit > 0:
            out = out[:limit]
        return out

    def count_matching(self, db: str, coll: str, filter: dict[str, Any] | None = None) -> int:
        if not filter:
            with self._lock:
                row = self._conn.execute(
                    "SELECT COUNT(*) FROM _fongodb_documents WHERE db_name = ? AND coll_name = ?",
                    (db, coll),
                ).fetchone()
                return row[0]
        return sum(1 for doc in self._all_docs(db, coll) if matches(doc, filter))

    def update_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        update: dict[str, Any],
        *,
        multi: bool = False,
        upsert: bool = False,
    ) -> dict[str, Any]:
        matched = 0
        modified = 0
        upserted_id: Any = None
        with self._lock:
            self._ensure_collection(db, coll)
            for doc in self._all_docs(db, coll):
                if not matches(doc, filter):
                    continue
                matched += 1
                new = apply_update(doc, update)
                if new != doc:
                    modified += 1
                    self._conn.execute(
                        "UPDATE _fongodb_documents SET doc = ? "
                        "WHERE db_name = ? AND coll_name = ? AND id_key = ?",
                        (bson.encode(new), db, coll, _id_key(new["_id"])),
                    )
                if not multi:
                    break
            if matched == 0 and upsert:
                seed: dict[str, Any] = {}
                for k, v in filter.items():
                    if not k.startswith("$") and not isinstance(v, dict):
                        seed[k] = v
                new = apply_update(seed, update)
                if "_id" not in new:
                    new["_id"] = bson.ObjectId()
                upserted_id = new["_id"]
                self._conn.execute(
                    "INSERT INTO _fongodb_documents(db_name, coll_name, id_key, doc) "
                    "VALUES (?, ?, ?, ?)",
                    (db, coll, _id_key(upserted_id), bson.encode(new)),
                )
        return {"matched": matched, "modified": modified, "upserted_id": upserted_id}

    def delete_matching(self, db: str, coll: str, filter: dict[str, Any], *, limit: int = 0) -> int:
        deleted = 0
        with self._lock:
            for doc in self._all_docs(db, coll):
                if not matches(doc, filter):
                    continue
                self._conn.execute(
                    "DELETE FROM _fongodb_documents "
                    "WHERE db_name = ? AND coll_name = ? AND id_key = ?",
                    (db, coll, _id_key(doc["_id"])),
                )
                deleted += 1
                if limit > 0 and deleted >= limit:
                    break
        return deleted

    def drop_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            existed = self.collection_exists(db, coll)
            self._conn.execute(
                "DELETE FROM _fongodb_documents WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            )
            self._conn.execute(
                "DELETE FROM _fongodb_collections WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            )
            return existed

    def drop_database(self, db: str) -> None:
        with self._lock:
            self._conn.execute("DELETE FROM _fongodb_documents WHERE db_name = ?", (db,))
            self._conn.execute("DELETE FROM _fongodb_collections WHERE db_name = ?", (db,))

    def list_collections(self, db: str) -> list[str]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT coll_name FROM _fongodb_collections WHERE db_name = ? ORDER BY coll_name",
                (db,),
            ).fetchall()
        return [r[0] for r in rows]

    def list_databases(self) -> list[str]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT DISTINCT db_name FROM _fongodb_collections ORDER BY db_name"
            ).fetchall()
        return [r[0] for r in rows]
