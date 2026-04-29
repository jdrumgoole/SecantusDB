"""SQLite-backed document store.

Indexes are a STOPGAP, not real indexing. ``createIndex`` records the
definition (so ``listIndexes`` returns it accurately) and enforces
``unique`` constraints by full-scanning the collection on every write.
There is no lookup acceleration: queries always full-scan the document
table and filter in Python. Replace this with a typed-sort-key BLOB
column scheme (per CLAUDE.md "Layer 3") if/when test suites grow large
enough to feel the difference.
"""

from __future__ import annotations

import sqlite3
import threading
from collections.abc import Iterable, Mapping
from decimal import Decimal, InvalidOperation
from typing import Any

import bson
from bson import Decimal128

from secantus.paths import get_path, has_path
from secantus.projection import apply_projection
from secantus.query import matches
from secantus.update import apply_update, find_positional_matches

_SCHEMA = """
CREATE TABLE IF NOT EXISTS _secantus_collections (
    db_name   TEXT NOT NULL,
    coll_name TEXT NOT NULL,
    options   BLOB,
    PRIMARY KEY (db_name, coll_name)
);

CREATE TABLE IF NOT EXISTS _secantus_documents (
    db_name   TEXT NOT NULL,
    coll_name TEXT NOT NULL,
    id_key    BLOB NOT NULL,
    doc       BLOB NOT NULL,
    PRIMARY KEY (db_name, coll_name, id_key)
);

CREATE TABLE IF NOT EXISTS _secantus_indexes (
    db_name    TEXT NOT NULL,
    coll_name  TEXT NOT NULL,
    index_name TEXT NOT NULL,
    key_spec   BLOB NOT NULL,
    options    BLOB,
    PRIMARY KEY (db_name, coll_name, index_name)
);
"""


class DuplicateKeyError(Exception):
    def __init__(self, doc_id: Any) -> None:
        super().__init__(f"duplicate _id: {doc_id!r}")
        self.doc_id = doc_id


_NUM_PREFIX = b"\x01n:"
_OTHER_PREFIX = b"\x01o:"


def _canon_decimal(d: Decimal) -> bytes | None:
    if not d.is_finite():
        return None
    if d == d.to_integral_value():
        return str(int(d)).encode()
    return format(d.normalize(), "f").encode()


def _canon_value(value: Any) -> bytes:
    if isinstance(value, bool):
        return _OTHER_PREFIX + bson.encode({"_": value})
    if isinstance(value, int):
        canon = _canon_decimal(Decimal(value))
        if canon is not None:
            return _NUM_PREFIX + canon
    elif isinstance(value, float):
        try:
            d = Decimal(repr(value))
        except (InvalidOperation, ValueError):
            d = None
        if d is not None:
            canon = _canon_decimal(d)
            if canon is not None:
                return _NUM_PREFIX + canon
    elif isinstance(value, Decimal128):
        try:
            canon = _canon_decimal(value.to_decimal())
        except (InvalidOperation, ValueError):
            canon = None
        if canon is not None:
            return _NUM_PREFIX + canon
    return _OTHER_PREFIX + bson.encode({"_": value})


def _id_key(doc_id: Any) -> bytes:
    return _canon_value(doc_id)


def _index_key(
    doc: Mapping[str, Any], key_spec: Mapping[str, Any], *, sparse: bool
) -> bytes | None:
    if sparse:
        for field in key_spec:
            if not has_path(dict(doc), field):
                return None
    return b"\x00".join(_canon_value(get_path(dict(doc), field)) for field in key_spec)


def _to_decimal(value: Any) -> Decimal:
    if isinstance(value, Decimal128):
        return value.to_decimal()
    if isinstance(value, float):
        return Decimal(repr(value))
    return Decimal(value)


def _bson_type_rank(value: Any) -> int:
    """Rank for MongoDB's cross-type sort order. Lower rank sorts first."""
    import datetime as _dt

    from bson import Binary, MaxKey, MinKey, ObjectId, Regex, Timestamp

    if isinstance(value, MinKey):
        return 1
    if value is None:
        return 2
    if isinstance(value, bool):
        return 9  # bool is a separate rank below numbers
    if isinstance(value, (int, float, Decimal128)):
        return 3
    if isinstance(value, str):
        return 4
    if isinstance(value, Mapping):
        return 5
    if isinstance(value, list):
        return 6
    if isinstance(value, (bytes, Binary)):
        return 7
    if isinstance(value, ObjectId):
        return 8
    if isinstance(value, _dt.datetime):
        return 10
    if isinstance(value, Timestamp):
        return 11
    if isinstance(value, Regex):
        return 12
    if isinstance(value, MaxKey):
        return 13
    return 5  # unknown -> object-rank


class _SortKey:
    __slots__ = ("val",)

    def __init__(self, val: Any) -> None:
        self.val = val

    def __lt__(self, other: _SortKey) -> bool:
        a, b = self.val, other.val
        ra = _bson_type_rank(a)
        rb = _bson_type_rank(b)
        if ra != rb:
            return ra < rb
        if a is None or b is None:
            return False
        if isinstance(a, Decimal128) or isinstance(b, Decimal128):
            try:
                ad = _to_decimal(a)
                bd = _to_decimal(b)
                return bool(ad < bd)
            except (InvalidOperation, ValueError):
                pass
        try:
            return bool(a < b)
        except TypeError:
            return type(a).__name__ < type(b).__name__

    def __eq__(self, other: object) -> bool:
        return isinstance(other, _SortKey) and self.val == other.val


def sort_docs(
    docs: list[dict[str, Any]], sort_spec: Mapping[str, Any] | None
) -> list[dict[str, Any]]:
    if not sort_spec:
        return docs
    result = list(docs)
    for field, direction in reversed(list(sort_spec.items())):
        result.sort(
            key=lambda d, f=field: _SortKey(get_path(d, f)),
            reverse=(int(direction) == -1),
        )
    return result


_ID_INDEX_NAME = "_id_"


class IndexConflict(Exception):
    def __init__(self, index_name: str, doc_id: Any) -> None:
        super().__init__(f"E11000 duplicate key error in index {index_name}: _id={doc_id!r}")
        self.index_name = index_name
        self.doc_id = doc_id


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
            "INSERT OR IGNORE INTO _secantus_collections(db_name, coll_name) VALUES (?, ?)",
            (db, coll),
        )

    def collection_exists(self, db: str, coll: str) -> bool:
        with self._lock:
            row = self._conn.execute(
                "SELECT 1 FROM _secantus_collections WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            ).fetchone()
            return row is not None

    def create_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            cursor = self._conn.execute(
                "INSERT OR IGNORE INTO _secantus_collections(db_name, coll_name) VALUES (?, ?)",
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
            indexes = self._unique_indexes(db, coll)
            for index, doc in enumerate(docs):
                if "_id" not in doc:
                    doc["_id"] = bson.ObjectId()
                key = _id_key(doc["_id"])
                conflict = self._unique_conflict(db, coll, doc, indexes, exclude_id_key=None)
                if conflict is not None:
                    errors.append(
                        {
                            "index": index,
                            "code": 11000,
                            "errmsg": (
                                f"E11000 duplicate key error in index {conflict}: "
                                f"_id={doc['_id']!r}"
                            ),
                        }
                    )
                    if ordered:
                        break
                    continue
                blob = bson.encode(doc)
                try:
                    self._conn.execute(
                        "INSERT INTO _secantus_documents(db_name, coll_name, id_key, doc) "
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
                "SELECT doc FROM _secantus_documents WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            ).fetchall()
        return [bson.decode(blob) for (blob,) in rows]

    def _all_docs_with_id_key(self, db: str, coll: str) -> list[tuple[dict[str, Any], bytes]]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT doc, id_key FROM _secantus_documents WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            ).fetchall()
        return [(bson.decode(blob), id_key) for (blob, id_key) in rows]

    def find_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        skip: int = 0,
        limit: int = 0,
        sort: Mapping[str, Any] | None = None,
        projection: Mapping[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        filter = filter or {}
        out: list[dict[str, Any]] = []
        for doc in self._all_docs(db, coll):
            if matches(doc, filter):
                out.append(doc)
        if sort:
            out = sort_docs(out, sort)
        if skip:
            out = out[skip:]
        if limit > 0:
            out = out[:limit]
        if projection:
            out = [apply_projection(d, projection) for d in out]
        return out

    def count_matching(self, db: str, coll: str, filter: dict[str, Any] | None = None) -> int:
        if not filter:
            with self._lock:
                row = self._conn.execute(
                    "SELECT COUNT(*) FROM _secantus_documents WHERE db_name = ? AND coll_name = ?",
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
        array_filters: list[dict[str, Any]] | None = None,
    ) -> dict[str, Any]:
        matched = 0
        modified = 0
        upserted_id: Any = None
        with self._lock:
            self._ensure_collection(db, coll)
            indexes = self._unique_indexes(db, coll)
            for doc in self._all_docs(db, coll):
                if not matches(doc, filter):
                    continue
                matched += 1
                pos = find_positional_matches(doc, filter)
                new = apply_update(doc, update, array_filters=array_filters, positional_matches=pos)
                if new != doc:
                    new_id_key = _id_key(new["_id"])
                    conflict = self._unique_conflict(
                        db, coll, new, indexes, exclude_id_key=_id_key(doc["_id"])
                    )
                    if conflict is not None:
                        raise IndexConflict(conflict, new["_id"])
                    modified += 1
                    self._conn.execute(
                        "UPDATE _secantus_documents SET doc = ? "
                        "WHERE db_name = ? AND coll_name = ? AND id_key = ?",
                        (bson.encode(new), db, coll, new_id_key),
                    )
                if not multi:
                    break
            if matched == 0 and upsert:
                seed: dict[str, Any] = {}
                for k, v in filter.items():
                    if not k.startswith("$") and not isinstance(v, dict):
                        seed[k] = v
                new = apply_update(seed, update, is_upsert=True, array_filters=array_filters)
                if "_id" not in new:
                    new["_id"] = bson.ObjectId()
                upserted_id = new["_id"]
                conflict = self._unique_conflict(db, coll, new, indexes, exclude_id_key=None)
                if conflict is not None:
                    raise IndexConflict(conflict, new["_id"])
                self._conn.execute(
                    "INSERT INTO _secantus_documents(db_name, coll_name, id_key, doc) "
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
                    "DELETE FROM _secantus_documents "
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
                "DELETE FROM _secantus_documents WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            )
            self._conn.execute(
                "DELETE FROM _secantus_indexes WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            )
            self._conn.execute(
                "DELETE FROM _secantus_collections WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            )
            return existed

    def drop_database(self, db: str) -> None:
        with self._lock:
            self._conn.execute("DELETE FROM _secantus_documents WHERE db_name = ?", (db,))
            self._conn.execute("DELETE FROM _secantus_indexes WHERE db_name = ?", (db,))
            self._conn.execute("DELETE FROM _secantus_collections WHERE db_name = ?", (db,))

    def rename_collection(
        self,
        src_db: str,
        src_coll: str,
        dst_db: str,
        dst_coll: str,
        *,
        drop_target: bool = False,
    ) -> tuple[bool, str | None]:
        with self._lock:
            if not self.collection_exists(src_db, src_coll):
                return False, f"source namespace does not exist: {src_db}.{src_coll}"
            if (src_db, src_coll) == (dst_db, dst_coll):
                return True, None
            if self.collection_exists(dst_db, dst_coll):
                if not drop_target:
                    return False, f"target namespace exists: {dst_db}.{dst_coll}"
                self._conn.execute(
                    "DELETE FROM _secantus_documents WHERE db_name = ? AND coll_name = ?",
                    (dst_db, dst_coll),
                )
                self._conn.execute(
                    "DELETE FROM _secantus_indexes WHERE db_name = ? AND coll_name = ?",
                    (dst_db, dst_coll),
                )
                self._conn.execute(
                    "DELETE FROM _secantus_collections WHERE db_name = ? AND coll_name = ?",
                    (dst_db, dst_coll),
                )
            self._conn.execute(
                "UPDATE _secantus_documents SET db_name = ?, coll_name = ? "
                "WHERE db_name = ? AND coll_name = ?",
                (dst_db, dst_coll, src_db, src_coll),
            )
            self._conn.execute(
                "UPDATE _secantus_indexes SET db_name = ?, coll_name = ? "
                "WHERE db_name = ? AND coll_name = ?",
                (dst_db, dst_coll, src_db, src_coll),
            )
            self._conn.execute(
                "INSERT OR IGNORE INTO _secantus_collections(db_name, coll_name) VALUES (?, ?)",
                (dst_db, dst_coll),
            )
            self._conn.execute(
                "DELETE FROM _secantus_collections WHERE db_name = ? AND coll_name = ?",
                (src_db, src_coll),
            )
            return True, None

    def list_collections(self, db: str) -> list[str]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT coll_name FROM _secantus_collections WHERE db_name = ? ORDER BY coll_name",
                (db,),
            ).fetchall()
        return [r[0] for r in rows]

    def list_databases(self) -> list[str]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT DISTINCT db_name FROM _secantus_collections ORDER BY db_name"
            ).fetchall()
        return [r[0] for r in rows]

    def create_index(
        self,
        db: str,
        coll: str,
        name: str,
        key_spec: Mapping[str, Any],
        options: Mapping[str, Any] | None = None,
    ) -> bool:
        if name == _ID_INDEX_NAME:
            return False
        options = dict(options or {})
        with self._lock:
            self._ensure_collection(db, coll)
            existing = self._conn.execute(
                "SELECT key_spec, options FROM _secantus_indexes "
                "WHERE db_name = ? AND coll_name = ? AND index_name = ?",
                (db, coll, name),
            ).fetchone()
            if existing is not None:
                return False
            if options.get("unique"):
                sparse = bool(options.get("sparse"))
                seen: dict[bytes, Any] = {}
                for d in self._all_docs(db, coll):
                    key = _index_key(d, key_spec, sparse=sparse)
                    if key is None:
                        continue
                    if key in seen:
                        raise IndexConflict(name, d.get("_id"))
                    seen[key] = d.get("_id")
            self._conn.execute(
                "INSERT INTO _secantus_indexes(db_name, coll_name, index_name, key_spec, options) "
                "VALUES (?, ?, ?, ?, ?)",
                (db, coll, name, bson.encode(dict(key_spec)), bson.encode(options)),
            )
            return True

    def list_indexes(self, db: str, coll: str) -> list[dict[str, Any]]:
        if not self.collection_exists(db, coll):
            return []
        out: list[dict[str, Any]] = [{"v": 2, "key": {"_id": 1}, "name": _ID_INDEX_NAME}]
        with self._lock:
            rows = self._conn.execute(
                "SELECT index_name, key_spec, options FROM _secantus_indexes "
                "WHERE db_name = ? AND coll_name = ? ORDER BY index_name",
                (db, coll),
            ).fetchall()
        for name, key_blob, opts_blob in rows:
            entry: dict[str, Any] = {
                "v": 2,
                "key": bson.decode(key_blob),
                "name": name,
            }
            if opts_blob:
                opts = bson.decode(opts_blob)
                for k, v in opts.items():
                    entry[k] = v
            out.append(entry)
        return out

    def drop_index(self, db: str, coll: str, name: str) -> bool:
        if name == _ID_INDEX_NAME:
            return False
        with self._lock:
            cursor = self._conn.execute(
                "DELETE FROM _secantus_indexes "
                "WHERE db_name = ? AND coll_name = ? AND index_name = ?",
                (db, coll, name),
            )
            return cursor.rowcount > 0

    def drop_all_indexes(self, db: str, coll: str) -> int:
        with self._lock:
            cursor = self._conn.execute(
                "DELETE FROM _secantus_indexes WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            )
            return cursor.rowcount

    def _unique_indexes(self, db: str, coll: str) -> list[tuple[str, dict[str, Any], bool]]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT index_name, key_spec, options FROM _secantus_indexes "
                "WHERE db_name = ? AND coll_name = ?",
                (db, coll),
            ).fetchall()
        out: list[tuple[str, dict[str, Any], bool]] = []
        for name, key_blob, opts_blob in rows:
            opts = bson.decode(opts_blob) if opts_blob else {}
            if opts.get("unique"):
                out.append((name, bson.decode(key_blob), bool(opts.get("sparse"))))
        return out

    def _unique_conflict(
        self,
        db: str,
        coll: str,
        candidate_doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool]],
        *,
        exclude_id_key: bytes | None,
    ) -> str | None:
        if not indexes:
            return None
        for name, key_spec, sparse in indexes:
            new_key = _index_key(candidate_doc, key_spec, sparse=sparse)
            if new_key is None:
                continue
            for other_doc, other_id_key in self._all_docs_with_id_key(db, coll):
                if exclude_id_key is not None and other_id_key == exclude_id_key:
                    continue
                if _index_key(other_doc, key_spec, sparse=sparse) == new_key:
                    return name
        return None
