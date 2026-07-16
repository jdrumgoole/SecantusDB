"""``unnest(array_col)`` as a FROM-clause table-function source — the lateral
``$unwind`` form (``SELECT … FROM t, unnest(t.tags) AS tag``).

Each row of the outer table is paired with one row per array element, exposed
under the alias column. Inner (comma / ``CROSS JOIN``) drops rows whose array is
empty; a ``LEFT JOIN … ON true`` keeps them with a NULL element.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, tags text[])")
    run(
        storage,
        session,
        "INSERT INTO t VALUES (1, ARRAY['a','b']), (2, ARRAY['c']), (3, ARRAY[]::text[])",
    )
    return storage


def test_comma_unnest(t, session):
    rows = run(t, session, "SELECT id, tag FROM t, unnest(t.tags) AS tag ORDER BY id, tag").rows
    assert rows == [(1, "a"), (1, "b"), (2, "c")]


def test_cross_join_unnest(t, session):
    rows = run(
        t, session, "SELECT id, tag FROM t CROSS JOIN unnest(t.tags) AS tag ORDER BY id, tag"
    ).rows
    assert rows == [(1, "a"), (1, "b"), (2, "c")]


def test_inner_drops_empty_array(t, session):
    # id=3 has an empty array, so it contributes no rows to the inner join.
    rows = run(t, session, "SELECT id FROM t, unnest(t.tags) AS tag").rows
    assert 3 not in [r[0] for r in rows]


def test_left_join_keeps_empty_array(t, session):
    rows = run(
        t, session, "SELECT id, tag FROM t LEFT JOIN unnest(t.tags) AS tag ON true ORDER BY id, tag"
    ).rows
    assert rows == [(1, "a"), (1, "b"), (2, "c"), (3, None)]


def test_unnest_literal_array(t, session):
    rows = run(t, session, "SELECT x FROM t, unnest(ARRAY[1,2]) AS x WHERE id = 1 ORDER BY x").rows
    assert rows == [(1,), (2,)]


def test_element_column_types_as_element(t, session):
    cols = run(t, session, "SELECT tag FROM t, unnest(t.tags) AS tag WHERE id = 1").columns
    assert cols[0].type_tag == "text"


def test_count_over_unnest(t, session):
    # Three tags across id=1 (2) + id=2 (1); id=3's empty array contributes none.
    assert run(t, session, "SELECT count(*) FROM t, unnest(t.tags) AS tag").rows == [(3,)]


def test_aggregate_group_over_unnest(t, session):
    rows = run(
        t,
        session,
        "SELECT id, count(*) FROM t, unnest(t.tags) AS tag GROUP BY id ORDER BY id",
    ).rows
    assert rows == [(1, 2), (2, 1)]


def test_column_alias_form(t, session):
    # unnest(...) AS x(v) names the element column v.
    rows = run(t, session, "SELECT v FROM t, unnest(t.tags) AS x(v) WHERE id = 2").rows
    assert rows == [("c",)]


# --------------------------------------------------------------------------- #
# jsonb_each lateral-join form (#160)
# --------------------------------------------------------------------------- #


@pytest.fixture
def docs(storage, session):
    run(storage, session, "CREATE TABLE d (id int PRIMARY KEY, doc jsonb)")
    run(storage, session, 'INSERT INTO d VALUES (1, \'{"a":1,"b":2}\'), (2, \'{"x":9}\')')
    return storage


def test_jsonb_each_lateral(docs, session):
    got = run(docs, session, "SELECT id, key, value FROM d, jsonb_each(doc) ORDER BY id, key").rows
    assert got == [(1, "a", 1), (1, "b", 2), (2, "x", 9)]


def test_jsonb_each_lateral_column_aliases(docs, session):
    got = run(
        docs, session, "SELECT id, k, v FROM d, jsonb_each(doc) AS e(k, v) ORDER BY id, k"
    ).rows
    assert got == [(1, "a", 1), (1, "b", 2), (2, "x", 9)]


def test_jsonb_each_lateral_where_on_value(docs, session):
    got = run(
        docs,
        session,
        "SELECT id, key FROM d, jsonb_each(doc) WHERE value::int > 1 ORDER BY id, key",
    ).rows
    assert got == [(1, "b"), (2, "x")]


def test_jsonb_each_lateral_empty_object_drops_row(docs, session):
    run(docs, session, "INSERT INTO d VALUES (3, '{}')")
    got = run(docs, session, "SELECT count(*) FROM d, jsonb_each(doc)").rows
    assert got == [(3,)]  # id=3 contributes no rows (inner join)


def test_jsonb_each_lateral_columns(docs, session):
    res = run(docs, session, "SELECT key, value FROM d, jsonb_each(doc) WHERE id = 2")
    assert [(c.name, c.type_tag) for c in res.columns] == [("key", "text"), ("value", "json")]
