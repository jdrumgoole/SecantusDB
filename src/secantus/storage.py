"""WiredTiger-backed document store.

WiredTiger is the default storage engine for MongoDB. We use the same
engine here so that on-disk semantics line up with what test code would
see against a real ``mongod``.

Indexes use a sidecar entries table (``table:secantus_index_entries``)
keyed by ``(db, coll, name, value_bytes, id_key)``. ``value_bytes`` comes
from ``_canon_value`` so byte-equal values collide — that's enough for
equality lookup and unique enforcement. Single-field equality filters
are routed through the index in ``find_matching``; everything else still
falls back to a full collection scan. Range / sort acceleration needs a
typed sort-key encoder (see backlog).
"""

from __future__ import annotations

import contextlib
import os
import shutil
import tempfile
import threading
from collections.abc import Iterable, Mapping
from decimal import Decimal, InvalidOperation
from typing import Any

import bson
import wiredtiger as wt
from bson import Decimal128

from secantus.paths import get_path, has_path
from secantus.projection import apply_projection
from secantus.query import matches
from secantus.sortkey import encode_compound, encode_value
from secantus.update import apply_update, find_positional_matches

_COLL_TABLE = "table:secantus_collections"
_DOC_TABLE = "table:secantus_documents"
_IDX_TABLE = "table:secantus_indexes"
_IDX_ENTRIES_TABLE = "table:secantus_index_entries"

_ENTRY_SEP = b"\x00\x00"


def _escape_kb(kb: bytes) -> bytes:
    """Order-preserving escape so ``\\x00\\x00`` is unambiguous as a separator."""
    return kb.replace(b"\x00", b"\x00\xff")


def _pack_entry(kb: bytes, id_key: bytes) -> bytes:
    """Pack a sortable index-entry payload into a single ``u`` column.

    WiredTiger length-prefixes ``u`` columns when they're not last in the
    key, which breaks lexicographic comparison. Packing both fields into
    one trailing ``u`` column lets the B-tree do the sort for us.
    """
    return _escape_kb(kb) + _ENTRY_SEP + id_key


def _unpack_entry(packed: bytes) -> tuple[bytes, bytes]:
    """Return ``(escaped_kb, id_key)`` from a packed entry."""
    sep = packed.find(_ENTRY_SEP)
    return packed[:sep], packed[sep + 2 :]


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
    """Byte-sortable encoding of a document's value for an index ``key_spec``.

    Single-field indexes use ``encode_value``; compound indexes use
    ``encode_compound`` with ``\\x00\\x00`` between components after each
    component's null bytes are escaped.
    """
    if sparse:
        for field in key_spec:
            if not has_path(dict(doc), field):
                return None
    fields = list(key_spec)
    if len(fields) == 1:
        return encode_value(get_path(dict(doc), fields[0]))
    return encode_compound([get_path(dict(doc), f) for f in fields])


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
        return 9
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
    return 5


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
        self._lock = threading.RLock()
        self._closed = False
        self._tempdir: str | None = None
        if path == ":memory:":
            self._tempdir = tempfile.mkdtemp(prefix="secantus_wt_")
            home = self._tempdir
            config = "create,in_memory=true"
        else:
            os.makedirs(path, exist_ok=True)
            home = path
            config = "create"
        self._conn = wt.wiredtiger_open(home, config)
        self._tls = threading.local()
        self._all_sessions: list[Any] = []
        boot = self._conn.open_session()
        try:
            boot.create(_COLL_TABLE, "key_format=SS,value_format=u")
            boot.create(_DOC_TABLE, "key_format=SSu,value_format=u")
            boot.create(_IDX_TABLE, "key_format=SSS,value_format=u")
            boot.create(_IDX_ENTRIES_TABLE, "key_format=SSSu,value_format=u")
        finally:
            boot.close()

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            for s in self._all_sessions:
                with contextlib.suppress(Exception):
                    s.close()
            self._all_sessions.clear()
            with contextlib.suppress(Exception):
                self._conn.close()
            if self._tempdir is not None:
                shutil.rmtree(self._tempdir, ignore_errors=True)
                self._tempdir = None

    def _session(self) -> Any:
        s = getattr(self._tls, "session", None)
        if s is None:
            s = self._conn.open_session()
            self._tls.session = s
            self._tls.cursors = {}
            with self._lock:
                self._all_sessions.append(s)
        return s

    def _cursor(self, table: str, *, overwrite: bool = True) -> Any:
        self._session()
        cursors: dict[tuple[str, bool], Any] = self._tls.cursors
        key = (table, overwrite)
        c = cursors.get(key)
        if c is None:
            cfg = None if overwrite else "overwrite=false"
            c = self._tls.session.open_cursor(table, None, cfg)
            cursors[key] = c
        else:
            c.reset()
        return c

    def _coll_options(self, db: str, coll: str) -> dict[str, Any] | None:
        c = self._cursor(_COLL_TABLE)
        c.set_key(db, coll)
        rc = c.search()
        if rc != 0:
            return None
        blob = bytes(c.get_value())
        return bson.decode(blob) if blob else {}

    def _ensure_collection(self, db: str, coll: str) -> None:
        c = self._cursor(_COLL_TABLE)
        c.set_key(db, coll)
        if c.search() == 0:
            return
        c.reset()
        c[db, coll] = b""

    def collection_exists(self, db: str, coll: str) -> bool:
        with self._lock:
            return self._coll_options(db, coll) is not None

    def create_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, coll)
            if c.search() == 0:
                return False
            c.reset()
            c[db, coll] = b""
            return True

    def _scan_docs(self, db: str, coll: str) -> Iterable[tuple[bytes, bytes]]:
        c = self._cursor(_DOC_TABLE)
        c.set_key(db, coll, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return
        if rc < 0 and c.next() != 0:
            return
        while True:
            k = c.get_key()
            if k[0] != db or k[1] != coll:
                return
            yield bytes(k[2]), bytes(c.get_value())
            if c.next() != 0:
                return

    def _all_docs(self, db: str, coll: str) -> list[dict[str, Any]]:
        with self._lock:
            return [bson.decode(blob) for _id_k, blob in self._scan_docs(db, coll)]

    def _all_docs_with_id_key(self, db: str, coll: str) -> list[tuple[dict[str, Any], bytes]]:
        with self._lock:
            return [(bson.decode(blob), id_k) for id_k, blob in self._scan_docs(db, coll)]

    def insert(
        self, db: str, coll: str, docs: Iterable[dict[str, Any]], *, ordered: bool = True
    ) -> tuple[int, list[dict[str, Any]]]:
        inserted = 0
        errors: list[dict[str, Any]] = []
        with self._lock:
            self._ensure_collection(db, coll)
            indexes = self._all_indexes(db, coll)
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
                doc_cur = self._cursor(_DOC_TABLE, overwrite=False)
                doc_cur.set_key(db, coll, key)
                doc_cur.set_value(blob)
                try:
                    doc_cur.insert()
                except wt.WiredTigerError:
                    errors.append(
                        {
                            "index": index,
                            "code": 11000,
                            "errmsg": f"E11000 duplicate key error: _id {doc['_id']!r}",
                        }
                    )
                    if ordered:
                        break
                    continue
                self._write_index_entries(db, coll, doc, indexes)
                inserted += 1
        return inserted, errors

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
        with self._lock:
            candidates = self._try_index_lookup(db, coll, filter)
            if candidates is None:
                candidates = [bson.decode(b) for _, b in self._scan_docs(db, coll)]
        out = [d for d in candidates if matches(d, filter)]
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
                return sum(1 for _ in self._scan_docs(db, coll))
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
            indexes = self._all_indexes(db, coll)
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
                    self._delete_index_entries(db, coll, doc, indexes)
                    doc_cur = self._cursor(_DOC_TABLE)
                    doc_cur[db, coll, new_id_key] = bson.encode(new)
                    self._write_index_entries(db, coll, new, indexes)
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
                doc_cur = self._cursor(_DOC_TABLE)
                doc_cur[db, coll, _id_key(upserted_id)] = bson.encode(new)
                self._write_index_entries(db, coll, new, indexes)
        return {"matched": matched, "modified": modified, "upserted_id": upserted_id}

    def delete_matching(self, db: str, coll: str, filter: dict[str, Any], *, limit: int = 0) -> int:
        deleted = 0
        with self._lock:
            indexes = self._all_indexes(db, coll)
            for doc in self._all_docs(db, coll):
                if not matches(doc, filter):
                    continue
                self._delete_index_entries(db, coll, doc, indexes)
                doc_cur = self._cursor(_DOC_TABLE)
                doc_cur.set_key(db, coll, _id_key(doc["_id"]))
                doc_cur.remove()
                deleted += 1
                if limit > 0 and deleted >= limit:
                    break
        return deleted

    @staticmethod
    def _table_kf(table: str) -> str:
        return {
            _COLL_TABLE: "SS",
            _DOC_TABLE: "SSu",
            _IDX_TABLE: "SSS",
            _IDX_ENTRIES_TABLE: "SSSu",
        }[table]

    @staticmethod
    def _smallest_for_kf(kf: str) -> tuple[Any, ...]:
        return tuple(b"" if c == "u" else "" for c in kf)

    def _collect_prefix(
        self, table: str, prefix: tuple[Any, ...]
    ) -> list[tuple[tuple[Any, ...], Any]]:
        c = self._cursor(table)
        kf = self._table_kf(table)
        seed = prefix + self._smallest_for_kf(kf)[len(prefix) :]
        c.set_key(*seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[tuple[tuple[Any, ...], Any]] = []
        while True:
            k = tuple(c.get_key())
            if k[: len(prefix)] != prefix:
                break
            v = c.get_value()
            out.append((k, bytes(v) if isinstance(v, (bytes, bytearray)) else v))
            if c.next() != 0:
                break
        return out

    def _delete_keys(self, table: str, keys: list[tuple[Any, ...]]) -> None:
        if not keys:
            return
        c = self._cursor(table)
        for k in keys:
            c.set_key(*k)
            c.remove()
            c.reset()

    def drop_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            existed = self._coll_options(db, coll) is not None
            for tbl in (_DOC_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE):
                rows = self._collect_prefix(tbl, (db, coll))
                self._delete_keys(tbl, [k for k, _ in rows])
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, coll)
            if c.search() == 0:
                c.remove()
            return existed

    def drop_database(self, db: str) -> None:
        with self._lock:
            for tbl in (_DOC_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE, _COLL_TABLE):
                rows = self._collect_prefix(tbl, (db,))
                self._delete_keys(tbl, [k for k, _ in rows])

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
            if self._coll_options(src_db, src_coll) is None:
                return False, f"source namespace does not exist: {src_db}.{src_coll}"
            if (src_db, src_coll) == (dst_db, dst_coll):
                return True, None
            if self._coll_options(dst_db, dst_coll) is not None:
                if not drop_target:
                    return False, f"target namespace exists: {dst_db}.{dst_coll}"
                for tbl in (_DOC_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE):
                    rows = self._collect_prefix(tbl, (dst_db, dst_coll))
                    self._delete_keys(tbl, [k for k, _ in rows])
                c = self._cursor(_COLL_TABLE)
                c.set_key(dst_db, dst_coll)
                if c.search() == 0:
                    c.remove()
            for tbl in (_DOC_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE):
                rows = self._collect_prefix(tbl, (src_db, src_coll))
                self._delete_keys(tbl, [k for k, _ in rows])
                c = self._cursor(tbl)
                for k, v in rows:
                    new_k = (dst_db, dst_coll) + k[2:]
                    c.set_key(*new_k)
                    c.set_value(v)
                    c.insert()
                    c.reset()
            ensure = self._cursor(_COLL_TABLE)
            ensure.set_key(dst_db, dst_coll)
            if ensure.search() != 0:
                ensure.reset()
                ensure[dst_db, dst_coll] = b""
            ensure.reset()
            ensure.set_key(src_db, src_coll)
            if ensure.search() == 0:
                ensure.remove()
            return True, None

    def list_collections(self, db: str) -> list[str]:
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, "")
            rc = c.search_near()
            if rc == wt.WT_NOTFOUND:
                return []
            if rc < 0 and c.next() != 0:
                return []
            out: list[str] = []
            while True:
                k = c.get_key()
                if k[0] != db:
                    break
                out.append(k[1])
                if c.next() != 0:
                    break
            return sorted(out)

    def list_databases(self) -> list[str]:
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            seen: set[str] = set()
            rc = c.next()
            while rc == 0:
                k = c.get_key()
                seen.add(k[0])
                rc = c.next()
            return sorted(seen)

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
            c = self._cursor(_IDX_TABLE)
            c.set_key(db, coll, name)
            if c.search() == 0:
                return False
            sparse = bool(options.get("sparse"))
            unique = bool(options.get("unique"))
            if unique:
                seen: dict[bytes, Any] = {}
                for d in self._all_docs(db, coll):
                    key = _index_key(d, key_spec, sparse=sparse)
                    if key is None:
                        continue
                    if key in seen:
                        raise IndexConflict(name, d.get("_id"))
                    seen[key] = d.get("_id")
            payload = bson.encode({"key": dict(key_spec), "options": options})
            c.reset()
            c[db, coll, name] = payload
            entry_cur = self._cursor(_IDX_ENTRIES_TABLE)
            for d in self._all_docs(db, coll):
                kb = _index_key(d, dict(key_spec), sparse=sparse)
                if kb is None:
                    continue
                entry_cur.reset()
                entry_cur[db, coll, name, _pack_entry(kb, _id_key(d["_id"]))] = b""
            return True

    def list_indexes(self, db: str, coll: str) -> list[dict[str, Any]]:
        with self._lock:
            if self._coll_options(db, coll) is None:
                return []
            out: list[dict[str, Any]] = [{"v": 2, "key": {"_id": 1}, "name": _ID_INDEX_NAME}]
            for name, key_spec, opts in self._iter_indexes(db, coll):
                entry: dict[str, Any] = {"v": 2, "key": key_spec, "name": name}
                for k, v in opts.items():
                    entry[k] = v
                out.append(entry)
            out.sort(key=lambda e: e["name"])
            return out

    def _iter_indexes(
        self, db: str, coll: str
    ) -> Iterable[tuple[str, dict[str, Any], dict[str, Any]]]:
        c = self._cursor(_IDX_TABLE)
        c.set_key(db, coll, "")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return
        if rc < 0 and c.next() != 0:
            return
        while True:
            k = c.get_key()
            if k[0] != db or k[1] != coll:
                return
            payload = bson.decode(bytes(c.get_value()))
            yield k[2], payload.get("key", {}), payload.get("options", {})
            if c.next() != 0:
                return

    def drop_index(self, db: str, coll: str, name: str) -> bool:
        if name == _ID_INDEX_NAME:
            return False
        with self._lock:
            c = self._cursor(_IDX_TABLE)
            c.set_key(db, coll, name)
            if c.search() != 0:
                return False
            c.remove()
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll, name))
            self._delete_keys(_IDX_ENTRIES_TABLE, [k for k, _ in entry_rows])
            return True

    def drop_all_indexes(self, db: str, coll: str) -> int:
        with self._lock:
            rows = self._collect_prefix(_IDX_TABLE, (db, coll))
            self._delete_keys(_IDX_TABLE, [k for k, _ in rows])
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll))
            self._delete_keys(_IDX_ENTRIES_TABLE, [k for k, _ in entry_rows])
            return len(rows)

    def _all_indexes(self, db: str, coll: str) -> list[tuple[str, dict[str, Any], bool, bool]]:
        """Every non-_id_ index: (name, key_spec, sparse, unique)."""
        out: list[tuple[str, dict[str, Any], bool, bool]] = []
        for name, key_spec, opts in list(self._iter_indexes(db, coll)):
            out.append((name, key_spec, bool(opts.get("sparse")), bool(opts.get("unique"))))
        return out

    def _write_index_entries(
        self,
        db: str,
        coll: str,
        doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
    ) -> None:
        if not indexes:
            return
        c = self._cursor(_IDX_ENTRIES_TABLE)
        id_k = _id_key(doc["_id"])
        for name, key_spec, sparse, _unique in indexes:
            kb = _index_key(doc, key_spec, sparse=sparse)
            if kb is None:
                continue
            c.reset()
            c[db, coll, name, _pack_entry(kb, id_k)] = b""

    def _delete_index_entries(
        self,
        db: str,
        coll: str,
        doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
    ) -> None:
        if not indexes:
            return
        c = self._cursor(_IDX_ENTRIES_TABLE)
        id_k = _id_key(doc["_id"])
        for name, key_spec, sparse, _unique in indexes:
            kb = _index_key(doc, key_spec, sparse=sparse)
            if kb is None:
                continue
            c.reset()
            c.set_key(db, coll, name, _pack_entry(kb, id_k))
            if c.search() == 0:
                c.remove()

    def _unique_conflict(
        self,
        db: str,
        coll: str,
        candidate_doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        *,
        exclude_id_key: bytes | None,
    ) -> str | None:
        if not indexes:
            return None
        c = self._cursor(_IDX_ENTRIES_TABLE)
        for name, key_spec, sparse, unique in indexes:
            if not unique:
                continue
            kb = _index_key(candidate_doc, key_spec, sparse=sparse)
            if kb is None:
                continue
            esc_kb = _escape_kb(kb)
            seed = esc_kb + _ENTRY_SEP
            c.reset()
            c.set_key(db, coll, name, seed)
            rc = c.search_near()
            if rc == wt.WT_NOTFOUND:
                continue
            if rc < 0 and c.next() != 0:
                continue
            while True:
                k = c.get_key()
                if (k[0], k[1], k[2]) != (db, coll, name):
                    break
                packed = bytes(k[3])
                row_esc, row_id = _unpack_entry(packed)
                if row_esc != esc_kb:
                    break
                if exclude_id_key is None or row_id != exclude_id_key:
                    return name
                if c.next() != 0:
                    break
        return None

    def _scan_index_for_id_keys(self, db: str, coll: str, name: str, kb: bytes) -> list[bytes]:
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_kb = _escape_kb(kb)
        c.set_key(db, coll, name, esc_kb + _ENTRY_SEP)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            row_esc, row_id = _unpack_entry(packed)
            if row_esc != esc_kb:
                break
            out.append(row_id)
            if c.next() != 0:
                break
        return out

    def _docs_by_id_keys(self, db: str, coll: str, id_keys: list[bytes]) -> list[dict[str, Any]]:
        if not id_keys:
            return []
        c = self._cursor(_DOC_TABLE)
        out: list[dict[str, Any]] = []
        for id_k in id_keys:
            c.reset()
            c.set_key(db, coll, id_k)
            if c.search() == 0:
                out.append(bson.decode(bytes(c.get_value())))
        return out

    _RANGE_OPS: tuple[str, ...] = ("$eq", "$gt", "$gte", "$lt", "$lte", "$in")

    def _try_index_lookup(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> list[dict[str, Any]] | None:
        if not filter or len(filter) != 1:
            return None
        field, value = next(iter(filter.items()))
        if field.startswith("$"):
            return None
        index_name: str | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if list(key_spec.keys()) == [field]:
                index_name = name
                break
        if index_name is None:
            return None
        if not isinstance(value, dict):
            kb = encode_value(value)
            id_keys = self._scan_index_for_id_keys(db, coll, index_name, kb)
            return self._docs_by_id_keys(db, coll, id_keys)
        if not value or not all(k.startswith("$") for k in value):
            # Mixed (some non-$ keys) means subdocument equality — skip.
            return None
        if not all(op in self._RANGE_OPS for op in value):
            return None
        if "$in" in value:
            if len(value) != 1 or not isinstance(value["$in"], list):
                return None
            seen: set[bytes] = set()
            id_keys = []
            for v in value["$in"]:
                if isinstance(v, dict):
                    return None
                kb = encode_value(v)
                for id_k in self._scan_index_for_id_keys(db, coll, index_name, kb):
                    if id_k not in seen:
                        seen.add(id_k)
                        id_keys.append(id_k)
            return self._docs_by_id_keys(db, coll, id_keys)
        lower: bytes | None = None
        lower_inclusive = True
        upper: bytes | None = None
        upper_inclusive = True
        for op, bound in value.items():
            if isinstance(bound, dict):
                return None
            if op == "$eq":
                kb = encode_value(bound)
                id_keys = self._scan_index_for_id_keys(db, coll, index_name, kb)
                return self._docs_by_id_keys(db, coll, id_keys)
            kb = encode_value(bound)
            if op == "$gt":
                lower, lower_inclusive = kb, False
            elif op == "$gte":
                lower, lower_inclusive = kb, True
            elif op == "$lt":
                upper, upper_inclusive = kb, False
            elif op == "$lte":
                upper, upper_inclusive = kb, True
        id_keys = self._range_scan_index(
            db, coll, index_name, lower, lower_inclusive, upper, upper_inclusive
        )
        return self._docs_by_id_keys(db, coll, id_keys)

    def _range_scan_index(
        self,
        db: str,
        coll: str,
        name: str,
        lower: bytes | None,
        lower_inclusive: bool,
        upper: bytes | None,
        upper_inclusive: bool,
    ) -> list[bytes]:
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_lower = _escape_kb(lower) if lower is not None else None
        esc_upper = _escape_kb(upper) if upper is not None else None
        seed = (esc_lower or b"") + _ENTRY_SEP if lower is not None else b""
        c.set_key(db, coll, name, seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            row_esc, row_id = _unpack_entry(packed)
            if esc_lower is not None and not lower_inclusive and row_esc == esc_lower:
                if c.next() != 0:
                    break
                continue
            if esc_upper is not None:
                if upper_inclusive:
                    if row_esc > esc_upper:
                        break
                elif row_esc >= esc_upper:
                    break
            out.append(row_id)
            if c.next() != 0:
                break
        return out
