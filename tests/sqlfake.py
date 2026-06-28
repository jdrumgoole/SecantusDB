"""In-memory ``Storage`` double for the SQL tests, backed by the real engines.

Shared by ``test_sql_spike.py`` and ``test_pgserver.py``. Query/update semantics
are the genuine pure-Python operator engines (``query.matches`` /
``update.apply_update``); only WiredTiger persistence is faked, so the SQL
translation is exercised against the same operators production uses. Not a test
module itself (no ``test_`` prefix), so pytest won't collect it.
"""

from __future__ import annotations

import contextlib
import copy
from dataclasses import dataclass

import bson

from secantus.paths import get_path
from secantus.projection import apply_projection
from secantus.query import matches
from secantus.update import apply_update


def _sortkey(value):
    return (0,) if value is None else (1, value)


def _sorted(docs, sort):
    items = list(docs)
    for field, direction in reversed(list(sort.items())):
        items.sort(key=lambda d, f=field: _sortkey(get_path(d, f)), reverse=(direction == -1))
    return items


class FakeStorage:
    """Minimal in-memory stand-in for ``Storage`` using the real engines."""

    def __init__(self) -> None:
        self.data: dict[tuple[str, str], list[dict]] = {}

    def _coll(self, db: str, coll: str) -> list[dict]:
        return self.data.setdefault((db, coll), [])

    def create_collection(self, db, coll, options=None):
        self.data.setdefault((db, coll), [])
        return True

    def drop_collection(self, db, coll):
        return self.data.pop((db, coll), None) is not None

    def list_collections(self, db):
        return sorted(c for (d, c) in self.data if d == db)

    def list_indexes(self, db, coll):
        # No secondary indexes — $lookup falls back to the hash-join path.
        return []

    def insert(self, db, coll, docs, *, ordered=True, journal=False):
        store = self._coll(db, coll)
        inserted = 0
        errors: list[dict] = []
        for i, doc in enumerate(docs):
            doc = copy.deepcopy(doc)
            if "_id" not in doc:
                doc["_id"] = bson.ObjectId()
            if any(d.get("_id") == doc["_id"] for d in store):
                errors.append(
                    {
                        "index": i,
                        "code": 11000,
                        "errmsg": f"E11000 duplicate key: _id {doc['_id']!r}",
                    }
                )
                if ordered:
                    break
                continue
            store.append(doc)
            inserted += 1
        return inserted, errors

    def find_matching(
        self, db, coll, filter=None, *, skip=0, limit=0, sort=None, projection=None, **kw
    ):
        # Read-only: must NOT create the collection (real Storage doesn't), so
        # reflection can tell an existing-but-empty collection from an unknown one.
        store = self.data.get((db, coll), [])
        out = [copy.deepcopy(d) for d in store if matches(d, filter or {})]
        if sort:
            out = _sorted(out, sort)
        if skip:
            out = out[skip:]
        if limit:
            out = out[:limit]
        if projection:
            out = [apply_projection(d, projection) for d in out]
        return out

    def update_matching(self, db, coll, filter, update, *, multi=False, **kw):
        store = self._coll(db, coll)
        matched = modified = 0
        for idx, d in enumerate(store):
            if matches(d, filter or {}):
                matched += 1
                new = apply_update(copy.deepcopy(d), update)
                if new != d:
                    store[idx] = new
                    modified += 1
                if not multi:
                    break
        return {"matched": matched, "modified": modified, "upserted_id": None, "did_upsert": False}

    def delete_matching(self, db, coll, filter, *, limit=0, **kw):
        store = self._coll(db, coll)
        keep: list[dict] = []
        deleted = 0
        for d in store:
            if (limit == 0 or deleted < limit) and matches(d, filter or {}):
                deleted += 1
            else:
                keep.append(d)
        self.data[(db, coll)] = keep
        return deleted

    # -- transaction emulation ---------------------------------------------- #
    # The real Storage installs the txn's WT session for the duration of
    # ``use_user_transaction``; the fake just snapshots the data at BEGIN and
    # restores it on abort, which gives the same atomic all-or-nothing semantics
    # the SQL transaction tests need (single-connection; isolation across
    # connections is the real engine's job).

    def begin_user_transaction(self):
        return _FakeTxn(snapshot=copy.deepcopy(self.data))

    @contextlib.contextmanager
    def use_user_transaction(self, handle):
        yield

    def commit_user_transaction(self, handle, **kw):
        return 0

    def abort_user_transaction(self, handle):
        self.data = handle.snapshot


@dataclass
class _FakeTxn:
    snapshot: dict
