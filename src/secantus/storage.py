"""WiredTiger-backed document store.

WiredTiger is the default storage engine for MongoDB. We use the same
engine here so that on-disk semantics line up with what test code would
see against a real ``mongod``.

Indexes use a sidecar entries table (``table:secantus_index_entries``)
with a single trailing ``u`` column packing
``escape(sortkey) + b"\\x00\\x00" + id_key``. The sortkey comes from
``secantus.sortkey`` (typed, byte-sortable BSON encoding), so the WT
B-tree gives us ordered access for free. ``find_matching`` routes a wide
range of filter shapes through the index — equality, ``$eq``, ``$in``,
``$gt``/``$gte``/``$lt``/``$lte`` on a single field, plus compound
indexes when filter fields cover a leading prefix (with optional range
on the next field). Sort-by-indexed-field walks the B-tree in order.
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
from secantus.sortkey import COMPOUND_SEP, encode_value_directed
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
    """Direction-aware byte-sortable encoding for an index ``key_spec``.

    Each field is encoded with ``encode_value_directed`` so ``-1``
    (descending) fields get bitwise-inverted bytes, making a forward
    B-tree walk yield values in descending order. Compound keys are
    joined with ``\\x00\\x00`` between components.
    """
    if sparse:
        for field in key_spec:
            if not has_path(dict(doc), field):
                return None
    fields = list(key_spec)
    if len(fields) == 1:
        d = int(key_spec[fields[0]])
        return encode_value_directed(get_path(dict(doc), fields[0]), d)
    parts = [encode_value_directed(get_path(dict(doc), f), int(key_spec[f])) for f in fields]
    return COMPOUND_SEP.join(parts)


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


class BadHint(Exception):
    """The ``hint`` passed to ``find_matching`` doesn't name an existing index."""


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
        hint: str | Mapping[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        filter = filter or {}
        in_sort_order = False
        with self._lock:
            sort_field, sort_dir = self._single_sort_spec(sort)
            if hint is not None:
                resolved = self._resolve_hint(db, coll, hint)
                candidates, in_sort_order = self._candidates_from_hint(
                    db, coll, resolved, sort_field, sort_dir
                )
            else:
                candidates = self._try_index_lookup(db, coll, filter)
                if candidates is not None and sort_field is not None:
                    if (
                        len(filter) == 1
                        and not next(iter(filter)).startswith("$")
                        and next(iter(filter)) == sort_field
                    ):
                        in_sort_order = True
                        idx = self._find_leading_field_index(db, coll, sort_field)
                        idx_dir = idx[1] if idx else 1
                        if sort_dir != idx_dir:
                            candidates = list(reversed(candidates))
                elif candidates is None and not filter and sort_field is not None:
                    idx = self._find_leading_field_index(db, coll, sort_field)
                    if idx is not None:
                        idx_name, idx_dir, _is_compound = idx
                        # If the index direction matches the sort direction,
                        # walk forward; if it's opposite, walk backward.
                        reverse = sort_dir != idx_dir
                        candidates = self._walk_index_in_order(db, coll, idx_name, reverse=reverse)
                        in_sort_order = True
                if candidates is None:
                    candidates = [bson.decode(b) for _, b in self._scan_docs(db, coll)]
        out = [d for d in candidates if matches(d, filter)]
        if sort and not in_sort_order:
            out = sort_docs(out, sort)
        if skip:
            out = out[skip:]
        if limit > 0:
            out = out[:limit]
        if projection:
            out = [apply_projection(d, projection) for d in out]
        return out

    def _resolve_hint(self, db: str, coll: str, hint: str | Mapping[str, Any]) -> str:
        """Resolve ``hint`` to an index name (or ``$natural``).

        ``hint`` may be an index name string, a key-spec dict matching an
        existing index, ``"$natural"``, or ``{"$natural": +/-1}``. Anything
        else raises ``BadHint`` so the command layer can return a Mongo
        ``BadValue`` error.
        """
        if isinstance(hint, str):
            if hint == "$natural":
                return "$natural"
            if hint == _ID_INDEX_NAME:
                return _ID_INDEX_NAME
            for name, _key_spec, _sparse, _unique in self._all_indexes(db, coll):
                if name == hint:
                    return name
            raise BadHint(f"hint {hint!r} does not correspond to an existing index")
        if isinstance(hint, Mapping):
            if list(hint) == ["$natural"]:
                return "$natural"
            if list(hint) == ["_id"] and int(hint["_id"]) == 1:
                return _ID_INDEX_NAME
            for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
                if dict(key_spec) == dict(hint):
                    return name
            raise BadHint(f"hint {dict(hint)!r} does not correspond to an existing index")
        raise BadHint(f"invalid hint type: {type(hint).__name__}")

    def _candidates_from_hint(
        self,
        db: str,
        coll: str,
        resolved: str,
        sort_field: str | None,
        sort_dir: int,
    ) -> tuple[list[dict[str, Any]], bool]:
        """Walk the index named by ``resolved`` (or full collection for $natural).

        Returns ``(candidates, in_sort_order)`` where ``in_sort_order`` is
        True when the hint's leading field matches the sort field — in
        which case ``find_matching`` skips the post-sort step.
        """
        if resolved == "$natural":
            return [bson.decode(b) for _, b in self._scan_docs(db, coll)], False
        if resolved == _ID_INDEX_NAME:
            # The doc table is keyed by id_key; iterating it gives entries
            # sorted by encoded _id, which matches the _id_ index walk.
            docs = [bson.decode(b) for _, b in self._scan_docs(db, coll)]
            in_order = sort_field == "_id"
            if in_order and sort_dir == -1:
                docs = list(reversed(docs))
            return docs, in_order
        # Find the index's leading field and its direction
        leading: str | None = None
        leading_dir = 1
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if name == resolved:
                first = next(iter(key_spec))
                leading = first
                leading_dir = int(key_spec[first])
                break
        candidates = self._walk_index_in_order(db, coll, resolved, reverse=False)
        in_order = sort_field is not None and sort_field == leading
        if in_order and sort_dir != leading_dir:
            candidates = list(reversed(candidates))
        return candidates, in_order

    @staticmethod
    def _single_sort_spec(sort: Mapping[str, Any] | None) -> tuple[str | None, int]:
        """Return ``(field, direction)`` if ``sort`` is single-field +/-1, else ``(None, 0)``."""
        if not sort or len(sort) != 1:
            return None, 0
        f, d = next(iter(sort.items()))
        if f.startswith("$"):
            return None, 0
        try:
            di = int(d)
        except (TypeError, ValueError):
            return None, 0
        if di not in (-1, 1):
            return None, 0
        return f, di

    def _single_field_index_for(self, db: str, coll: str, field: str) -> tuple[str, int] | None:
        """Return ``(index_name, direction)`` for a single-field index on
        ``field``, or ``None`` if no such index exists. Direction is the
        index's stored sort direction (`+1` for ASC, `-1` for DESC)."""
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if list(key_spec.keys()) == [field]:
                d = int(key_spec[field])
                if d in (1, -1):
                    return name, d
        return None

    def _walk_index_in_order(
        self, db: str, coll: str, name: str, *, reverse: bool = False
    ) -> list[dict[str, Any]]:
        c = self._cursor(_IDX_ENTRIES_TABLE)
        c.set_key(db, coll, name, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        id_keys: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            _esc, row_id = _unpack_entry(packed)
            id_keys.append(row_id)
            if c.next() != 0:
                break
        if reverse:
            id_keys.reverse()
        return self._docs_by_id_keys(db, coll, id_keys)

    def explain_plan(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        sort: Mapping[str, Any] | None = None,
        hint: str | Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Plan summary for what ``find_matching`` would do with these args.

        No execution; mirrors the same routing decisions. Returns
        ``{"kind": "COLLSCAN"}`` or ``{"kind": "IXSCAN", "index_name",
        "key_pattern", "direction"}``. ``direction`` is ``"forward"``
        unless a sort spec inverts it relative to the chosen index.
        """
        filter = filter or {}
        with self._lock:
            sort_field, sort_dir = self._single_sort_spec(sort)
            if hint is not None:
                try:
                    resolved = self._resolve_hint(db, coll, hint)
                except BadHint:
                    return {"kind": "COLLSCAN"}
                if resolved == "$natural":
                    return {"kind": "COLLSCAN"}
                if resolved == _ID_INDEX_NAME:
                    direction = "forward"
                    if sort_field == "_id" and sort_dir == -1:
                        direction = "backward"
                    return {
                        "kind": "IXSCAN",
                        "index_name": _ID_INDEX_NAME,
                        "key_pattern": {"_id": 1},
                        "direction": direction,
                    }
                key_spec = self._key_spec_for(db, coll, resolved)
                if key_spec is None:
                    return {"kind": "COLLSCAN"}
                return self._make_ixscan_plan(resolved, key_spec, sort_field, sort_dir)
            picked = self._pick_index_for_filter(db, coll, filter)
            if picked is not None:
                name, key_spec = picked
                return self._make_ixscan_plan(name, key_spec, sort_field, sort_dir)
            if not filter and sort_field is not None:
                idx = self._find_leading_field_index(db, coll, sort_field)
                if idx is not None:
                    name, _idx_dir, _is_compound = idx
                    key_spec = self._key_spec_for(db, coll, name)
                    if key_spec is not None:
                        return self._make_ixscan_plan(name, key_spec, sort_field, sort_dir)
            return {"kind": "COLLSCAN"}

    def _key_spec_for(self, db: str, coll: str, name: str) -> dict[str, Any] | None:
        for n, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if n == name:
                return dict(key_spec)
        return None

    def _pick_index_for_filter(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> tuple[str, dict[str, Any]] | None:
        """Mirror ``_try_index_lookup``'s index-selection (no execution)."""
        if not filter:
            return None
        if any(f.startswith("$") for f in filter):
            return None
        if all(not isinstance(v, dict) for v in filter.values()):
            picked = self._pick_compound_eq_index(db, coll, filter)
            if picked is not None:
                return picked
        if len(filter) >= 2:
            picked = self._pick_compound_range_index(db, coll, filter)
            if picked is not None:
                return picked
        if len(filter) != 1:
            return None
        field, value = next(iter(filter.items()))
        idx_match = self._find_leading_field_index(db, coll, field)
        if idx_match is None:
            return None
        if isinstance(value, dict):
            if not value or not all(k.startswith("$") for k in value):
                return None
            if not all(op in self._RANGE_OPS for op in value):
                return None
        name, _direction, _is_compound = idx_match
        key_spec = self._key_spec_for(db, coll, name)
        if key_spec is None:
            return None
        return name, key_spec

    @staticmethod
    def _make_ixscan_plan(
        name: str,
        key_spec: Mapping[str, Any],
        sort_field: str | None,
        sort_dir: int,
    ) -> dict[str, Any]:
        direction = "forward"
        if sort_field is not None and sort_field in key_spec:
            idx_dir = int(key_spec[sort_field])
            if sort_dir != 0 and sort_dir != idx_dir:
                direction = "backward"
        return {
            "kind": "IXSCAN",
            "index_name": name,
            "key_pattern": dict(key_spec),
            "direction": direction,
        }

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

    def _scan_index_for_id_keys(
        self, db: str, coll: str, name: str, kb: bytes, *, prefix: bool = False
    ) -> list[bytes]:
        """Walk the index entries for ``name`` matching ``kb``.

        With ``prefix=False`` (default), only rows whose ``escaped_kb`` is
        exactly equal to ``escape(kb)`` are returned — equality lookup.
        With ``prefix=True``, any row whose ``escaped_kb`` starts with
        ``escape(kb)`` is returned — compound-prefix lookup.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_kb = _escape_kb(kb)
        seed = esc_kb if prefix else esc_kb + _ENTRY_SEP
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
            if prefix:
                if not row_esc.startswith(esc_kb):
                    break
            elif row_esc != esc_kb:
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
        if not filter:
            return None
        if any(f.startswith("$") for f in filter):
            return None
        # Bare-equality filters of any size can use a compound index whose
        # leading fields cover the filter set.
        if all(not isinstance(v, dict) for v in filter.values()):
            result = self._try_compound_eq_lookup(db, coll, filter)
            if result is not None:
                return result
        # Compound prefix + trailing operator field (eq fields then range/in).
        if len(filter) >= 2:
            result = self._try_compound_range_lookup(db, coll, filter)
            if result is not None:
                return result
        if len(filter) != 1:
            return None
        field, value = next(iter(filter.items()))
        idx_match = self._find_leading_field_index(db, coll, field)
        if idx_match is None:
            return None
        return self._lookup_via_leading_field(db, coll, idx_match, value)

    def _find_leading_field_index(
        self, db: str, coll: str, field: str
    ) -> tuple[str, int, bool] | None:
        """Best index whose leading field is ``field``.

        Returns ``(name, direction, is_compound)``. Single-field indexes
        win over compound (tighter scan, no separator math). All fields
        must be ASC or DESC.
        """
        compound_fallback: tuple[str, int, bool] | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            idx_fields = list(key_spec)
            if not idx_fields or idx_fields[0] != field:
                continue
            if any(int(key_spec[f]) not in (1, -1) for f in idx_fields):
                continue
            d = int(key_spec[field])
            if len(idx_fields) == 1:
                return name, d, False
            if compound_fallback is None:
                compound_fallback = (name, d, True)
        return compound_fallback

    def _lookup_via_leading_field(
        self,
        db: str,
        coll: str,
        idx_match: tuple[str, int, bool],
        value: Any,
    ) -> list[dict[str, Any]] | None:
        name, direction, is_compound = idx_match
        if not isinstance(value, dict):
            return self._docs_by_id_keys(
                db,
                coll,
                self._eq_id_keys_via_leading(db, coll, name, direction, is_compound, value),
            )
        if not value or not all(k.startswith("$") for k in value):
            return None
        if not all(op in self._RANGE_OPS for op in value):
            return None
        if "$in" in value:
            if len(value) != 1 or not isinstance(value["$in"], list):
                return None
            seen: set[bytes] = set()
            id_keys: list[bytes] = []
            for v in value["$in"]:
                if isinstance(v, dict):
                    return None
                for id_k in self._eq_id_keys_via_leading(db, coll, name, direction, is_compound, v):
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
                return self._docs_by_id_keys(
                    db,
                    coll,
                    self._eq_id_keys_via_leading(db, coll, name, direction, is_compound, bound),
                )
            kb = encode_value_directed(bound, direction)
            # Operator semantics flip when stored bytes are inverted: in a
            # DESC index, "x > 5" means we want stored bytes < enc_desc(5).
            effective_op = op
            if direction == -1:
                effective_op = {"$gt": "$lt", "$gte": "$lte", "$lt": "$gt", "$lte": "$gte"}[op]
            if effective_op == "$gt":
                lower, lower_inclusive = kb, False
            elif effective_op == "$gte":
                lower, lower_inclusive = kb, True
            elif effective_op == "$lt":
                upper, upper_inclusive = kb, False
            elif effective_op == "$lte":
                upper, upper_inclusive = kb, True
        if is_compound:
            id_keys = self._range_scan_index_leading(
                db, coll, name, lower, lower_inclusive, upper, upper_inclusive
            )
        else:
            id_keys = self._range_scan_index(
                db, coll, name, lower, lower_inclusive, upper, upper_inclusive
            )
        return self._docs_by_id_keys(db, coll, id_keys)

    def _eq_id_keys_via_leading(
        self,
        db: str,
        coll: str,
        name: str,
        direction: int,
        is_compound: bool,
        value: Any,
    ) -> list[bytes]:
        kb = encode_value_directed(value, direction)
        if is_compound:
            return self._scan_index_for_id_keys(db, coll, name, kb + COMPOUND_SEP, prefix=True)
        return self._scan_index_for_id_keys(db, coll, name, kb)

    def _pick_compound_eq_index(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> tuple[str, dict[str, Any]] | None:
        """Find the index that ``_try_compound_eq_lookup`` would walk for ``filter``.

        Returns ``(name, key_spec)`` of the chosen index, or ``None`` if no
        index covers the filter as a leading prefix. Pure picker — does
        not scan.
        """
        filter_fields = set(filter)
        best: tuple[str, dict[str, Any]] | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            idx_fields = list(key_spec.keys())
            if any(int(key_spec[f]) not in (1, -1) for f in idx_fields):
                continue
            if len(idx_fields) < len(filter_fields):
                continue
            if set(idx_fields[: len(filter_fields)]) != filter_fields:
                continue
            if best is None or (len(list(best[1])) > len(idx_fields)):
                best = (name, dict(key_spec))
            if len(idx_fields) == len(filter_fields):
                break
        return best

    def _try_compound_eq_lookup(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> list[dict[str, Any]] | None:
        """Bare-equality filter against a compound (or single-field) index prefix.

        Picks an index whose leading fields (set-wise) match the filter's
        fields, and runs an equality (full-cover) or prefix
        (strict-leading-prefix) scan against it. Per-field index direction
        is honoured by encoding each value with ``encode_value_directed``.
        """
        picked = self._pick_compound_eq_index(db, coll, filter)
        if picked is None:
            return None
        name, key_spec = picked
        idx_fields = list(key_spec)
        filter_fields = set(filter)
        prefix_fields = idx_fields[: len(filter_fields)]
        parts = [encode_value_directed(filter[f], int(key_spec[f])) for f in prefix_fields]
        kb = COMPOUND_SEP.join(parts) if len(parts) > 1 else parts[0]
        if len(filter_fields) == len(idx_fields):
            id_keys = self._scan_index_for_id_keys(db, coll, name, kb)
        else:
            kb = kb + COMPOUND_SEP
            id_keys = self._scan_index_for_id_keys(db, coll, name, kb, prefix=True)
        return self._docs_by_id_keys(db, coll, id_keys)

    def _partition_compound_range_filter(
        self, filter: dict[str, Any]
    ) -> tuple[dict[str, Any], str, dict[str, Any]] | None:
        """Split a filter into ``(eq_fields, operator_field, operator_ops)``.

        Returns ``None`` if the filter doesn't fit the compound-range
        shape (any number of bare-equality fields plus exactly one
        operator-form field whose ops are all in ``_RANGE_OPS``).
        """
        eq_fields: dict[str, Any] = {}
        operator_field: str | None = None
        operator_ops: dict[str, Any] | None = None
        for f, v in filter.items():
            if isinstance(v, dict):
                if not v or not all(k.startswith("$") for k in v):
                    return None
                if not all(op in self._RANGE_OPS for op in v):
                    return None
                if operator_field is not None:
                    return None
                operator_field = f
                operator_ops = v
            else:
                eq_fields[f] = v
        if operator_field is None or not eq_fields:
            return None
        if operator_field in eq_fields:
            return None
        return eq_fields, operator_field, operator_ops or {}

    def _pick_compound_range_index(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> tuple[str, dict[str, Any]] | None:
        """Find the index that ``_try_compound_range_lookup`` would walk."""
        parts = self._partition_compound_range_filter(filter)
        if parts is None:
            return None
        eq_fields, operator_field, _operator_ops = parts
        eq_set = set(eq_fields)
        target_eq_count = len(eq_set)
        best: tuple[str, dict[str, Any]] | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            idx_fields = list(key_spec.keys())
            if any(int(key_spec[f]) not in (1, -1) for f in idx_fields):
                continue
            if len(idx_fields) <= target_eq_count:
                continue
            if set(idx_fields[:target_eq_count]) != eq_set:
                continue
            if idx_fields[target_eq_count] != operator_field:
                continue
            if best is None or len(list(best[1])) > len(idx_fields):
                best = (name, dict(key_spec))
            if len(idx_fields) == target_eq_count + 1:
                break
        return best

    def _try_compound_range_lookup(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> list[dict[str, Any]] | None:
        """Compound-prefix lookup with a trailing operator field.

        Filters of the form ``{a: 5, b: 10, c: {$gt: 20}}`` (any number of
        leading bare-equality fields followed by exactly one operator-form
        field) walk the compound index by pinning the prefix from the
        equalities and applying the operator's bounds to the next field.
        """
        parts = self._partition_compound_range_filter(filter)
        if parts is None:
            return None
        eq_fields, operator_field, operator_ops = parts
        picked = self._pick_compound_range_index(db, coll, filter)
        if picked is None:
            return None
        name, key_spec = picked
        idx_fields = list(key_spec)
        target_eq_count = len(eq_fields)
        eq_field_names = idx_fields[:target_eq_count]
        op_dir = int(key_spec[operator_field])
        eq_parts = [encode_value_directed(eq_fields[f], int(key_spec[f])) for f in eq_field_names]
        prefix_kb = COMPOUND_SEP.join(eq_parts) if len(eq_parts) > 1 else eq_parts[0]
        prefix_with_sep = prefix_kb + COMPOUND_SEP
        if "$in" in operator_ops:
            if len(operator_ops) != 1 or not isinstance(operator_ops["$in"], list):
                return None
            seen: set[bytes] = set()
            id_keys: list[bytes] = []
            for v in operator_ops["$in"]:
                if isinstance(v, dict):
                    return None
                kb = prefix_with_sep + encode_value_directed(v, op_dir)
                use_prefix = len(idx_fields) > target_eq_count + 1
                inner_kb = kb + COMPOUND_SEP if use_prefix else kb
                for id_k in self._scan_index_for_id_keys(
                    db, coll, name, inner_kb, prefix=use_prefix
                ):
                    if id_k not in seen:
                        seen.add(id_k)
                        id_keys.append(id_k)
            return self._docs_by_id_keys(db, coll, id_keys)
        if "$eq" in operator_ops:
            if len(operator_ops) != 1:
                return None
            kb = prefix_with_sep + encode_value_directed(operator_ops["$eq"], op_dir)
            use_prefix = len(idx_fields) > target_eq_count + 1
            inner_kb = kb + COMPOUND_SEP if use_prefix else kb
            id_keys = self._scan_index_for_id_keys(db, coll, name, inner_kb, prefix=use_prefix)
            return self._docs_by_id_keys(db, coll, id_keys)
        lower: bytes | None = None
        lower_inclusive = True
        upper: bytes | None = None
        upper_inclusive = True
        for op, bound in operator_ops.items():
            if isinstance(bound, dict):
                return None
            full = prefix_with_sep + encode_value_directed(bound, op_dir)
            effective_op = op
            if op_dir == -1:
                effective_op = {"$gt": "$lt", "$gte": "$lte", "$lt": "$gt", "$lte": "$gte"}[op]
            if effective_op == "$gt":
                lower, lower_inclusive = full, False
            elif effective_op == "$gte":
                lower, lower_inclusive = full, True
            elif effective_op == "$lt":
                upper, upper_inclusive = full, False
            elif effective_op == "$lte":
                upper, upper_inclusive = full, True
            else:
                return None
        id_keys = self._range_scan_index(
            db,
            coll,
            name,
            lower,
            lower_inclusive,
            upper,
            upper_inclusive,
            prefix=prefix_with_sep,
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
        *,
        prefix: bytes | None = None,
    ) -> list[bytes]:
        """Range-scan the index entries for ``name``.

        Optional ``prefix`` constrains the scan to entries whose escaped
        kb starts with ``escape(prefix)`` — used by compound-index
        prefix+range queries where leading equalities pin part of the kb.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_prefix = _escape_kb(prefix) if prefix is not None else None
        esc_lower = _escape_kb(lower) if lower is not None else None
        esc_upper = _escape_kb(upper) if upper is not None else None
        if esc_lower is not None:
            seed = esc_lower + _ENTRY_SEP
        elif esc_prefix is not None:
            seed = esc_prefix
        else:
            seed = b""
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
            if esc_prefix is not None and not row_esc.startswith(esc_prefix):
                break
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

    def _range_scan_index_leading(
        self,
        db: str,
        coll: str,
        name: str,
        lower: bytes | None,
        lower_inclusive: bool,
        upper: bytes | None,
        upper_inclusive: bool,
    ) -> list[bytes]:
        """Range-scan a compound index using only its leading field.

        Each row's escaped kb is
        ``escape(enc(leading)) + escape(COMPOUND_SEP) + escape(enc(trailing...))``.
        Boundary detection uses ``startswith(esc_X + esc_compound_sep)`` to
        identify rows whose leading field equals ``X`` — the terminator
        bytes of an escaped numeric encoding can overlap with the start of
        the escaped compound separator, so a literal find/split on the
        separator is unreliable.
        """
        esc_compound_sep = _escape_kb(COMPOUND_SEP)
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_lower = _escape_kb(lower) if lower is not None else None
        esc_upper = _escape_kb(upper) if upper is not None else None
        seed = esc_lower if esc_lower is not None else b""
        c.set_key(db, coll, name, seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        lower_eq_prefix = esc_lower + esc_compound_sep if esc_lower is not None else None
        upper_eq_prefix = esc_upper + esc_compound_sep if esc_upper is not None else None
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            row_esc, row_id = _unpack_entry(packed)
            if (
                lower_eq_prefix is not None
                and not lower_inclusive
                and row_esc.startswith(lower_eq_prefix)
            ):
                if c.next() != 0:
                    break
                continue
            if esc_upper is not None:
                if upper_inclusive:
                    if row_esc > esc_upper and not row_esc.startswith(upper_eq_prefix):
                        break
                elif row_esc >= esc_upper:
                    break
            out.append(row_id)
            if c.next() != 0:
                break
        return out
