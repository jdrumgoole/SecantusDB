"""End-to-end tests for the embedded SQL engine (P0 spike).

These drive ``secantus.sql.run_sql`` against an in-memory ``FakeStorage`` whose
query/update semantics are the **real** pure-Python operator engines
(``query.matches`` / ``update.apply_update``). So the SQL-to-Mongo translation
is exercised against the same engines the production ``Storage`` uses — only the
WiredTiger persistence layer is faked, which is exactly the part that can't
build in a network-restricted dev box.

In CI (where the WiredTiger extension is present) the stub below is a no-op, so
these run unchanged; a follow-up phase adds a parallel suite against the real
``Storage``.
"""

from __future__ import annotations

import copy
import datetime as _dt
from decimal import Decimal

import bson
import pytest

from secantus.paths import get_path
from secantus.projection import apply_projection
from secantus.query import matches
from secantus.sql import SQLError, run_sql
from secantus.update import apply_update

DB = "testdb"


def _sortkey(value):
    # NULLs sort first; otherwise sort by the value itself (columns are
    # single-typed in these tests, so intra-column comparison is total).
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
        store = self._coll(db, coll)
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


@pytest.fixture
def storage():
    return FakeStorage()


def sql(storage, statement):
    """Run one statement and return its single result."""
    return run_sql(storage, DB, statement)[0]


def _make_users(storage):
    sql(storage, "CREATE TABLE users (id bigint primary key, name text, age int, active boolean)")
    sql(
        storage,
        "INSERT INTO users (id, name, age, active) VALUES "
        "(1, 'alice', 30, true), (2, 'bob', 17, false), (3, 'carol', 42, true)",
    )


# --------------------------------------------------------------------------- #


def test_create_insert_select_all(storage):
    _make_users(storage)
    res = sql(storage, "SELECT id, name, age, active FROM users ORDER BY id")
    assert res.command_tag == "SELECT 3"
    assert [c.name for c in res.columns] == ["id", "name", "age", "active"]
    assert res.rows == [
        (1, "alice", 30, True),
        (2, "bob", 17, False),
        (3, "carol", 42, True),
    ]


def test_select_star_expands_columns(storage):
    _make_users(storage)
    res = sql(storage, "SELECT * FROM users ORDER BY id LIMIT 1")
    assert [c.name for c in res.columns] == ["id", "name", "age", "active"]
    assert res.rows == [(1, "alice", 30, True)]


def test_pk_maps_to_id_field(storage):
    _make_users(storage)
    # The PK column is stored as the document _id.
    assert storage.find_matching(DB, "users", {"_id": bson.Int64(1)})[0]["name"] == "alice"


def test_where_comparisons_and_and(storage):
    _make_users(storage)
    res = sql(storage, "SELECT name FROM users WHERE age >= 18 AND active = true ORDER BY name")
    assert res.rows == [("alice",), ("carol",)]


def _names(storage, where):
    return {r[0] for r in sql(storage, f"SELECT name FROM users WHERE {where}").rows}


def test_where_or_not_in_between(storage):
    _make_users(storage)
    assert _names(storage, "age < 18 OR age > 40") == {"bob", "carol"}
    assert _names(storage, "id IN (1, 3)") == {"alice", "carol"}
    assert _names(storage, "age BETWEEN 18 AND 40") == {"alice"}
    assert _names(storage, "NOT active = true") == {"bob"}


def test_where_like(storage):
    _make_users(storage)
    assert {r[0] for r in sql(storage, "SELECT name FROM users WHERE name LIKE 'a%'").rows} == {
        "alice"
    }
    assert {r[0] for r in sql(storage, "SELECT name FROM users WHERE name LIKE '_ob'").rows} == {
        "bob"
    }


def test_order_desc_limit_offset(storage):
    _make_users(storage)
    res = sql(storage, "SELECT name FROM users ORDER BY age DESC LIMIT 2 OFFSET 1")
    assert res.rows == [("alice",), ("bob",)]


def test_count_star(storage):
    _make_users(storage)
    res = sql(storage, "SELECT COUNT(*) FROM users WHERE active = true")
    assert res.columns[0].name == "count"
    assert res.rows == [(2,)]


def test_is_null_and_not_null(storage):
    sql(storage, "CREATE TABLE t (id bigint primary key, note text)")
    sql(storage, "INSERT INTO t (id, note) VALUES (1, 'hi'), (2, NULL)")
    assert sql(storage, "SELECT id FROM t WHERE note IS NULL").rows == [(2,)]
    assert sql(storage, "SELECT id FROM t WHERE note IS NOT NULL").rows == [(1,)]


def test_update(storage):
    _make_users(storage)
    res = sql(storage, "UPDATE users SET age = 18, name = 'robert' WHERE id = 2")
    assert res.command_tag == "UPDATE 1"
    assert sql(storage, "SELECT name, age FROM users WHERE id = 2").rows == [("robert", 18)]


def test_delete(storage):
    _make_users(storage)
    res = sql(storage, "DELETE FROM users WHERE age < 18")
    assert res.command_tag == "DELETE 1"
    assert sql(storage, "SELECT COUNT(*) FROM users").rows == [(2,)]


def test_drop_table(storage):
    _make_users(storage)
    assert sql(storage, "DROP TABLE users").command_tag == "DROP TABLE"
    with pytest.raises(SQLError) as ei:
        sql(storage, "SELECT * FROM users")
    assert ei.value.sqlstate == "42P01"


def test_numeric_and_timestamp_coercion(storage):
    sql(storage, "CREATE TABLE m (id bigint primary key, price numeric, at timestamptz)")
    sql(storage, "INSERT INTO m (id, price, at) VALUES (1, 19.99, '2020-01-02T03:04:05Z')")
    # numeric stored as Decimal128, surfaced as Decimal; timestamptz as datetime.
    stored = storage.find_matching(DB, "m", {})[0]
    assert isinstance(stored["price"], bson.Decimal128)
    assert isinstance(stored["at"], _dt.datetime)
    row = sql(storage, "SELECT price, at FROM m").rows[0]
    assert row[0] == Decimal("19.99")
    assert row[1] == _dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=_dt.timezone.utc)


def test_date_literal_comparison_coerced(storage):
    sql(storage, "CREATE TABLE ev (id bigint primary key, at timestamptz)")
    sql(
        storage,
        "INSERT INTO ev (id, at) VALUES (1, '2020-01-01T00:00:00Z'), (2, '2021-06-01T00:00:00Z')",
    )
    res = sql(storage, "SELECT id FROM ev WHERE at >= '2021-01-01T00:00:00Z'")
    assert res.rows == [(2,)]


def test_duplicate_pk_raises_unique_violation(storage):
    _make_users(storage)
    with pytest.raises(SQLError) as ei:
        sql(storage, "INSERT INTO users (id, name, age, active) VALUES (1, 'dup', 1, true)")
    assert ei.value.sqlstate == "23505"


def test_not_null_violation(storage):
    sql(storage, "CREATE TABLE nn (id bigint primary key, name text not null)")
    with pytest.raises(SQLError) as ei:
        sql(storage, "INSERT INTO nn (id) VALUES (1)")
    assert ei.value.sqlstate == "23502"


def test_undefined_column(storage):
    _make_users(storage)
    with pytest.raises(SQLError) as ei:
        sql(storage, "SELECT nope FROM users")
    assert ei.value.sqlstate == "42703"


def test_unsupported_join_reports_feature_error(storage):
    _make_users(storage)
    with pytest.raises(SQLError) as ei:
        sql(storage, "SELECT u.name FROM users u JOIN users v ON u.id = v.id")
    assert ei.value.sqlstate == "0A000"


def test_duplicate_table_and_if_not_exists(storage):
    sql(storage, "CREATE TABLE x (id bigint primary key)")
    with pytest.raises(SQLError) as ei:
        sql(storage, "CREATE TABLE x (id bigint primary key)")
    assert ei.value.sqlstate == "42P07"
    # IF NOT EXISTS is a no-op, not an error.
    assert sql(storage, "CREATE TABLE IF NOT EXISTS x (id bigint primary key)").command_tag == (
        "CREATE TABLE"
    )


def test_multi_statement_returns_one_result_each(storage):
    results = run_sql(
        storage,
        DB,
        "CREATE TABLE q (id bigint primary key, n int);"
        "INSERT INTO q (id, n) VALUES (1, 10);"
        "SELECT n FROM q;",
    )
    assert [r.command_tag for r in results] == ["CREATE TABLE", "INSERT 0 1", "SELECT 1"]
    assert results[-1].rows == [(10,)]
