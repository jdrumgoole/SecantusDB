"""``string_agg`` and the boolean aggregates (``bool_and`` / ``bool_or`` /
``every``).

``string_agg(expr, sep)`` concatenates non-NULL values with the separator; it
lowers to a ``$push`` accumulator plus a ``$reduce`` in the group ``$project`` that
joins the pushed array (skipping NULL elements). ``bool_and`` / ``every`` reduce to
``$min`` over booleans (all-true), ``bool_or`` to ``$max`` (any-true).
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
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


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, grp text, name text, active bool)")
    run(
        storage,
        session,
        "INSERT INTO t VALUES (1,'a','x',true),(2,'a','y',false),"
        "(3,'a',NULL,true),(4,'b','z',true)",
    )
    return storage


# -- string_agg --------------------------------------------------------------- #


def test_string_agg_grouped(t, session):
    rows = run(
        t, session, "SELECT grp, string_agg(name, ',') FROM t GROUP BY grp ORDER BY grp"
    ).rows
    assert rows == [("a", "x,y"), ("b", "z")]  # NULL name in group a is skipped


def test_string_agg_no_group(t, session):
    assert run(t, session, "SELECT string_agg(name, '-') FROM t").rows == [("x-y-z",)]


def test_string_agg_result_types_as_text(t, session):
    cols = run(t, session, "SELECT string_agg(name, ',') AS s FROM t").columns
    assert cols[0].type_tag == "text"


def test_string_agg_all_null_is_null(storage, session):
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, g int, v text)")
    run(storage, session, "INSERT INTO u VALUES (1, 1, NULL), (2, 1, NULL)")
    assert run(storage, session, "SELECT string_agg(v, ',') FROM u GROUP BY g").rows == [(None,)]


def test_string_agg_over_join(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, grp text, name text)")
    run(storage, session, "INSERT INTO t VALUES (1,'a','x'),(2,'a','y'),(3,'b','z')")
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, t_id int)")
    run(storage, session, "INSERT INTO u VALUES (10,1),(20,2),(30,3)")
    rows = run(
        storage,
        session,
        "SELECT t.grp, string_agg(t.name, '|') FROM u JOIN t ON u.t_id=t.id "
        "GROUP BY t.grp ORDER BY t.grp",
    ).rows
    assert rows == [("a", "x|y"), ("b", "z")]


def test_string_agg_rollup(t, session):
    rows = run(
        t, session, "SELECT grp, string_agg(name, ',') FROM t GROUP BY ROLLUP(grp) ORDER BY grp"
    ).rows
    assert rows == [("a", "x,y"), ("b", "z"), (None, "x,y,z")]


# -- boolean aggregates ------------------------------------------------------- #


def test_bool_and(t, session):
    assert run(
        t, session, "SELECT grp, bool_and(active) FROM t GROUP BY grp ORDER BY grp"
    ).rows == [
        ("a", False),
        ("b", True),
    ]


def test_bool_or(t, session):
    assert run(t, session, "SELECT grp, bool_or(active) FROM t GROUP BY grp ORDER BY grp").rows == [
        ("a", True),
        ("b", True),
    ]


def test_every_is_bool_and(t, session):
    assert run(t, session, "SELECT grp, every(active) FROM t GROUP BY grp ORDER BY grp").rows == [
        ("a", False),
        ("b", True),
    ]


# -- ordered-set aggregates now supported (see test_sql_ordered_set_agg.py) ---- #


def test_percentile_cont_supported(t, session):
    # ids 1,2,3,4 -> median rank 1.5 -> 2.5.
    assert run(
        t, session, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY id) FROM t"
    ).rows == [(2.5,)]


def test_mode_supported(t, session):
    # ids are all distinct -> the smallest.
    assert run(t, session, "SELECT mode() WITHIN GROUP (ORDER BY id) FROM t").rows == [(1,)]


class TestNestedStringAgg:
    """A ``string_agg`` inside an expression keeps its in-call ORDER BY (it was
    dropped -- a silent wrong order), works over a JOIN (it was a 0A000), and
    ``||`` over a non-text operand casts it to text (it was an XX000). Each
    expected value is PostgreSQL 15's."""

    @pytest.fixture
    def t(self, storage, session):
        run(storage, session, "create table nsa(id int primary key, v text)")
        run(storage, session, "insert into nsa values (1,'a'),(2,'b'),(3,null),(4,'c')")
        return storage

    @pytest.mark.parametrize(
        ("sql", "expected"),
        [
            ("select coalesce(string_agg(v, ',' order by id desc), 'x') from nsa", "c,b,a"),
            ("select 'x' || string_agg(v, ',' order by v desc) from nsa", "xc,b,a"),
            (
                "select 'x' || string_agg(id || v, ',' order by v desc nulls last, id) from nsa",
                "x4c,2b,1a",
            ),
            ("select string_agg(id || '=' || v, ',' order by id) from nsa", "1=a,2=b,4=c"),
            (
                "select 'x' || string_agg(o.v, ',' order by o.id desc) "
                "from nsa o join nsa n on n.id = o.id",
                "xc,b,a",
            ),
            (
                "select coalesce(string_agg(o.v, ','), 'none') "
                "from nsa o join nsa n on n.id = o.id and false",
                "none",
            ),
        ],
    )
    def test_matches_postgres(self, t, session, sql, expected):
        assert run(t, session, sql).rows == [(expected,)]
