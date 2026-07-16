"""Range aggregates + multirange (#106): the range algebra operators
(``*`` intersection / ``+`` union / ``-`` difference / ``-|-`` adjacency),
``range_merge``, the ``range_agg`` aggregate, and multirange types
(``int4multirange`` etc.).
"""

from __future__ import annotations

import pytest

from secantus.sql import ranges as R
from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure ranges.py algebra
# --------------------------------------------------------------------------- #


def _r(lo, hi, b="[)", t="int4range"):
    return R.make_range(lo, hi, b, t)


def test_merge_disjoint_spans_gap():
    assert R.render(R.merge(_r(1, 5), _r(10, 15))) == "[1,15)"


def test_intersect_overlap():
    assert R.render(R.intersect(_r(1, 10), _r(5, 20))) == "[5,10)"


def test_intersect_disjoint_is_empty():
    assert R.is_empty(R.intersect(_r(1, 5), _r(10, 15)))


def test_union_overlap():
    assert R.render(R.union(_r(1, 10), _r(5, 20))) == "[1,20)"


def test_union_adjacent():
    assert R.render(R.union(_r(1, 5), _r(5, 10))) == "[1,10)"


def test_union_disjoint_raises():
    with pytest.raises(R.RangeError):
        R.union(_r(1, 5), _r(10, 15))


def test_adjacent():
    assert R.adjacent(_r(1, 5), _r(5, 10)) is True
    assert R.adjacent(_r(1, 6), _r(5, 10)) is False  # overlap
    assert R.adjacent(_r(1, 5), _r(6, 10)) is False  # gap


def test_difference_left_and_right():
    assert R.render(R.difference(_r(1, 10), _r(5, 20))) == "[1,5)"
    assert R.render(R.difference(_r(5, 20), _r(1, 10))) == "[10,20)"


def test_difference_split_raises():
    with pytest.raises(R.RangeError):
        R.difference(_r(1, 20), _r(5, 10))


def test_make_multirange_coalesces():
    mr = R.make_multirange([_r(1, 5), _r(10, 15), _r(3, 8)])
    # No space after the separator — Postgres prints ``{[1,8),[10,15)}``.
    assert R.render_multirange(mr) == "{[1,8),[10,15)}"


def test_parse_multirange_roundtrip():
    mr = R.parse_multirange("{[1,5), [10,20)}", "int4multirange", int)
    assert R.render_multirange(mr) == "{[1,5),[10,20)}"


def test_parse_empty_multirange():
    assert R.render_multirange(R.parse_multirange("{}", "int4multirange", int)) == "{}"


# --------------------------------------------------------------------------- #
# SQL surface
# --------------------------------------------------------------------------- #


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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, g int, r int4range)")
    for i, (g, lo, hi) in enumerate([(1, 1, 5), (1, 3, 8), (1, 20, 25), (2, 1, 2)], start=1):
        run(storage, session, f"INSERT INTO t VALUES ({i}, {g}, int4range({lo},{hi}))")
    return storage


def test_intersection_operator(storage, session):
    assert val(storage, session, "SELECT int4range(1,10) * int4range(5,20)") == R.make_range(
        5, 10, "[)", "int4range"
    )


def test_union_operator(storage, session):
    assert val(storage, session, "SELECT int4range(1,10) + int4range(5,20)") == R.make_range(
        1, 20, "[)", "int4range"
    )


def test_difference_operator(storage, session):
    assert val(storage, session, "SELECT int4range(5,20) - int4range(1,10)") == R.make_range(
        10, 20, "[)", "int4range"
    )


def test_operators_typed_range(storage, session):
    assert col(storage, session, "SELECT int4range(1,10) * int4range(5,20)").type_tag == "int4range"


def test_range_merge(storage, session):
    assert val(storage, session, "SELECT range_merge(int4range(1,5), int4range(10,15))") == (
        R.make_range(1, 15, "[)", "int4range")
    )


def test_range_merge_typed_range(storage, session):
    c = col(storage, session, "SELECT range_merge(int4range(1,5), int4range(10,15))")
    assert c.type_tag == "int4range"


def test_adjacent_operator(storage, session):
    assert val(storage, session, "SELECT int4range(1,5) -|- int4range(5,9)") is True
    assert val(storage, session, "SELECT int4range(1,5) -|- int4range(6,9)") is False


def test_adjacent_typed_bool(storage, session):
    assert col(storage, session, "SELECT int4range(1,5) -|- int4range(5,9)").type_tag == "bool"


def test_multirange_constructor(storage, session):
    assert val(
        storage, session, "SELECT int4multirange(int4range(1,5), int4range(10,15))"
    ) == R.make_multirange(
        [R.make_range(1, 5, "[)", "int4range"), R.make_range(10, 15, "[)", "int4range")]
    )


def test_multirange_constructor_typed(storage, session):
    c = col(storage, session, "SELECT int4multirange(int4range(1,5))")
    assert c.type_tag == "int4multirange"


def test_multirange_cast(storage, session):
    assert val(storage, session, "SELECT '{[1,5), [10,20)}'::int4multirange") == R.make_multirange(
        [R.make_range(1, 5, "[)", "int4range"), R.make_range(10, 20, "[)", "int4range")]
    )


def test_multirange_column_roundtrip(storage, session):
    run(storage, session, "CREATE TABLE m (id int PRIMARY KEY, mr int4multirange)")
    run(
        storage,
        session,
        "INSERT INTO m VALUES (1, int4multirange(int4range(1,5), int4range(10,15)))",
    )
    assert val(storage, session, "SELECT mr FROM m WHERE id = 1") == R.make_multirange(
        [R.make_range(1, 5, "[)", "int4range"), R.make_range(10, 15, "[)", "int4range")]
    )


def test_range_agg_whole_table(t, session):
    # [1,5) and [3,8) coalesce to [1,8); [20,25) is separate; group 2's [1,2) merges in.
    assert val(t, session, "SELECT range_agg(r) AS m FROM t") == R.make_multirange(
        [
            R.make_range(1, 8, "[)", "int4range"),
            R.make_range(20, 25, "[)", "int4range"),
        ]
    )


def test_range_agg_grouped(t, session):
    rows = run(t, session, "SELECT g, range_agg(r) AS m FROM t GROUP BY g ORDER BY g").rows
    assert [tuple(r) for r in rows] == [
        (
            1,
            R.make_multirange(
                [R.make_range(1, 8, "[)", "int4range"), R.make_range(20, 25, "[)", "int4range")]
            ),
        ),
        (2, R.make_multirange([R.make_range(1, 2, "[)", "int4range")])),
    ]


def test_range_agg_typed_multirange(t, session):
    assert col(t, session, "SELECT range_agg(r) AS m FROM t").type_tag == "int4multirange"
