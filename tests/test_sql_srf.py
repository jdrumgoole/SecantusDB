"""generate_series + base-less FROM-clause set-returning functions (#125):
``generate_series`` (SELECT-list and FROM forms), ``FROM unnest(...)`` /
``jsonb_array_elements`` / ``regexp_split_to_table``, ``WITH ORDINALITY``, and
column/table aliases — all with projection / WHERE / ORDER BY / LIMIT.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


def _run(sql, sess=None, st=None):
    sess = sess or Session(database=DB)
    if st is not None:
        return run_sql(st, DB, sql, session=sess)[-1]
    st = Storage(":memory:")
    try:
        return run_sql(st, DB, sql, session=sess)[-1]
    finally:
        st.close()


def _rows(sql):
    return _run(sql).rows


def _cols(sql):
    r = _run(sql)
    return [c.name for c in r.columns]


# --------------------------------------------------------------------------- #
# generate_series
# --------------------------------------------------------------------------- #


def test_generate_series_fromless():
    assert _rows("SELECT generate_series(1, 5)") == [(1,), (2,), (3,), (4,), (5,)]
    assert _cols("SELECT generate_series(1, 5)") == ["generate_series"]


def test_generate_series_from():
    assert _rows("SELECT * FROM generate_series(1, 4)") == [(1,), (2,), (3,), (4,)]


def test_generate_series_step():
    assert _rows("SELECT * FROM generate_series(1, 10, 2)") == [(1,), (3,), (5,), (7,), (9,)]


def test_generate_series_negative_step():
    assert _rows("SELECT * FROM generate_series(5, 1, -2)") == [(5,), (3,), (1,)]


def test_generate_series_empty_and_single():
    assert _rows("SELECT * FROM generate_series(5, 1)") == []
    assert _rows("SELECT * FROM generate_series(3, 3)") == [(3,)]


def test_generate_series_zero_step_errors():
    with pytest.raises(errors.SQLError) as exc:
        _rows("SELECT * FROM generate_series(1, 5, 0)")
    assert exc.value.sqlstate == "22023"


def test_generate_series_count():
    assert _rows("SELECT count(*) FROM generate_series(1, 100)") == [(100,)]


# --------------------------------------------------------------------------- #
# Aliases, WHERE / ORDER BY / LIMIT
# --------------------------------------------------------------------------- #


def test_table_alias_names_column():
    # FROM generate_series(1,5) AS g -> the single column is named g.
    assert _cols("SELECT * FROM generate_series(1, 3) AS g") == ["g"]
    assert _rows("SELECT g FROM generate_series(1, 3) AS g") == [(1,), (2,), (3,)]


def test_column_alias():
    assert _cols("SELECT * FROM generate_series(1, 3) AS g(n)") == ["n"]
    assert _rows("SELECT n FROM generate_series(1, 3) AS g(n) WHERE n > 1") == [(2,), (3,)]


def test_order_by_and_limit():
    assert _rows("SELECT * FROM generate_series(1, 5) AS g ORDER BY g DESC LIMIT 2") == [(5,), (4,)]


# --------------------------------------------------------------------------- #
# WITH ORDINALITY
# --------------------------------------------------------------------------- #


def test_with_ordinality():
    r = _run("SELECT * FROM generate_series(10, 30, 10) WITH ORDINALITY")
    assert [c.name for c in r.columns] == ["generate_series", "ordinality"]
    assert r.rows == [(10, 1), (20, 2), (30, 3)]


def test_with_ordinality_aliased():
    r = _run("SELECT * FROM generate_series(1, 3) WITH ORDINALITY AS t(v, ord)")
    assert [c.name for c in r.columns] == ["v", "ord"]
    assert r.rows == [(1, 1), (2, 2), (3, 3)]


# --------------------------------------------------------------------------- #
# Other FROM-clause SRFs
# --------------------------------------------------------------------------- #


def test_from_unnest():
    assert _rows("SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x") == [(10,), (20,), (30,)]
    assert _cols("SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x") == ["x"]


def test_from_regexp_split_to_table():
    assert _rows("SELECT * FROM regexp_split_to_table('a,b,c', ',') AS p") == [
        ("a",),
        ("b",),
        ("c",),
    ]


def test_from_jsonb_array_elements():
    assert _rows("SELECT * FROM jsonb_array_elements('[1,2,3]'::jsonb) AS e") == [(1,), (2,), (3,)]


def test_from_jsonb_object_keys():
    got = _rows('SELECT * FROM jsonb_object_keys(\'{"a":1,"b":2}\'::jsonb) AS k')
    assert sorted(r[0] for r in got) == ["a", "b"]


# --------------------------------------------------------------------------- #
# Errors
# --------------------------------------------------------------------------- #


def test_date_generate_series_not_supported():
    sql = "SELECT * FROM generate_series('2026-01-01'::date, '2026-01-03'::date, '1 day'::interval)"
    with pytest.raises(errors.SQLError) as exc:
        _rows(sql)
    assert exc.value.sqlstate == "0A000"
