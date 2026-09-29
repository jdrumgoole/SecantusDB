"""Array manipulation functions — ``array_append`` / ``array_prepend`` /
``array_cat`` / ``array_position`` / ``array_remove`` / ``array_to_string`` — plus
``array_agg`` populating a declared array column via ``INSERT … SELECT``.

Arrays are native BSON lists, so these evaluate in Python over the list; a NULL
array is treated as empty (``array_append(NULL, x) -> {x}``) the way Postgres does.
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
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, tags text[], nums int[])")
    run(storage, session, "INSERT INTO t VALUES (1, ARRAY['a','b','c'], ARRAY[10,20,30])")
    return storage


def test_array_append(t, session):
    assert run(t, session, "SELECT array_append(tags, 'd') FROM t").rows == [
        (["a", "b", "c", "d"],)
    ]


def test_array_prepend(t, session):
    assert run(t, session, "SELECT array_prepend('z', tags) FROM t").rows == [
        (["z", "a", "b", "c"],)
    ]


def test_array_cat(t, session):
    assert run(t, session, "SELECT array_cat(nums, ARRAY[40,50]) FROM t").rows == [
        ([10, 20, 30, 40, 50],)
    ]


def test_array_position_found(t, session):
    assert run(t, session, "SELECT array_position(tags, 'b') FROM t").rows == [(2,)]


def test_array_position_missing_is_null(t, session):
    assert run(t, session, "SELECT array_position(tags, 'zzz') FROM t").rows == [(None,)]


def test_array_remove(t, session):
    assert run(t, session, "SELECT array_remove(tags, 'b') FROM t").rows == [(["a", "c"],)]


def test_array_to_string(t, session):
    assert run(t, session, "SELECT array_to_string(tags, '-') FROM t").rows == [("a-b-c",)]


def test_array_to_string_skips_nulls(t, session):
    run(t, session, "INSERT INTO t VALUES (2, ARRAY['x',NULL,'y'], ARRAY[1])")
    assert run(t, session, "SELECT array_to_string(tags, ',') FROM t WHERE id=2").rows == [("x,y",)]


def test_array_to_string_null_string(t, session):
    run(t, session, "INSERT INTO t VALUES (2, ARRAY['x',NULL,'y'], ARRAY[1])")
    assert run(t, session, "SELECT array_to_string(tags, ',', 'NA') FROM t WHERE id=2").rows == [
        ("x,NA,y",)
    ]


def test_append_result_types_as_array(t, session):
    cols = run(t, session, "SELECT array_append(tags, 'd') AS x FROM t").columns
    assert cols[0].type_tag == "text[]"


def test_position_result_types_as_int(t, session):
    cols = run(t, session, "SELECT array_position(tags, 'a') AS p FROM t").columns
    assert cols[0].type_tag == "int4"


def test_append_null_array_is_singleton(storage, session):
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, tags text[])")
    run(storage, session, "INSERT INTO u (id) VALUES (1)")  # tags omitted -> NULL
    assert run(storage, session, "SELECT array_append(tags, 'a') FROM u").rows == [(["a"],)]


def test_array_agg_into_declared_array_column(storage, session):
    run(storage, session, "CREATE TABLE g (grp int PRIMARY KEY, items int[])")
    run(storage, session, "CREATE TABLE src (id int PRIMARY KEY, grp int, v int)")
    run(storage, session, "INSERT INTO src VALUES (1,1,10),(2,1,20),(3,2,30)")
    run(
        storage,
        session,
        "INSERT INTO g (grp, items) SELECT grp, array_agg(v) FROM src GROUP BY grp",
    )
    assert run(storage, session, "SELECT grp, items FROM g ORDER BY grp").rows == [
        (1, [10, 20]),
        (2, [30]),
    ]


# --- multi-dimensional array introspection (#153) -------------------------- #

_M = "ARRAY[[1,2,3],[4,5,6]]"  # a 2x3 array


def test_array_ndims_multidim(storage, session):
    assert run(storage, session, f"SELECT array_ndims({_M})").rows == [(2,)]
    assert run(storage, session, "SELECT array_ndims(ARRAY[1,2,3])").rows == [(1,)]


def test_array_length_per_dimension(storage, session):
    assert run(storage, session, f"SELECT array_length({_M}, 1)").rows == [(2,)]
    assert run(storage, session, f"SELECT array_length({_M}, 2)").rows == [(3,)]
    assert run(storage, session, f"SELECT array_length({_M}, 3)").rows == [(None,)]


def test_array_upper_lower_multidim(storage, session):
    assert run(storage, session, f"SELECT array_upper({_M}, 2)").rows == [(3,)]
    assert run(storage, session, f"SELECT array_lower({_M}, 2)").rows == [(1,)]
    assert run(storage, session, f"SELECT array_upper({_M}, 3)").rows == [(None,)]


def test_cardinality_counts_all_elements(storage, session):
    assert run(storage, session, f"SELECT cardinality({_M})").rows == [(6,)]
    assert run(storage, session, "SELECT cardinality(ARRAY[1,2,3])").rows == [(3,)]


def test_array_dims_text(storage, session):
    res = run(storage, session, f"SELECT array_dims({_M})")
    assert res.rows == [("[1:2][1:3]",)]
    assert res.columns[0].type_tag == "text"


def test_multidim_introspection_types(storage, session):
    for fn, tag in [("array_ndims", "int4"), ("cardinality", "int4")]:
        assert run(storage, session, f"SELECT {fn}({_M})").columns[0].type_tag == tag


def test_multidim_array_column_roundtrip_and_funcs(storage, session):
    run(storage, session, "CREATE TABLE grids (id int PRIMARY KEY, g int[][])")
    run(storage, session, f"INSERT INTO grids VALUES (1, {_M})")
    assert run(storage, session, "SELECT g FROM grids").rows == [([[1, 2, 3], [4, 5, 6]],)]
    assert run(storage, session, "SELECT g[2][3] FROM grids").rows == [(6,)]
    assert run(storage, session, "SELECT array_ndims(g), cardinality(g) FROM grids").rows == [
        (2, 6)
    ]
    assert run(storage, session, "SELECT array_length(g, 2) FROM grids").rows == [(3,)]


# --- PostgreSQL fidelity, measured on 14.13 via tools/probes/pg_differential.py
#
# Each expectation below is PostgreSQL's own answer to the same statement. The
# cases here are the ones where this server had a DIFFERENT answer rather than
# no answer -- a wrong value or a bogus error, neither of which a refusal-shaped
# test would have caught.


def test_array_cat_of_two_nulls_is_null_not_the_empty_array(storage, session):
    """A NULL side is taken as the empty one -- that is what lets `array_cat`
    fold over a nullable accumulator -- but when EVERY side is NULL the answer
    is NULL. Coercing each operand first returned `{}`, a value PostgreSQL
    never gives here and one that `IS NULL` then disagrees about."""
    row = run(
        storage,
        session,
        "SELECT array_cat(NULL::int[], ARRAY[3]), array_cat(ARRAY[1], NULL::int[]),"
        " array_cat(NULL::int[], NULL::int[])",
    ).rows[0]
    assert row == ([3], [1], None)


def test_array_position_honours_its_start_argument(storage, session):
    """The third argument was parsed and then DROPPED, so
    `array_position(ARRAY[1,2,3,2], 2, 3)` answered 2 where PostgreSQL answers
    4 -- a wrong number rather than an error, which is why nothing caught it.

    sqlglot files that argument under `zero_based`, a slot name it reuses;
    reading `expressions` (the obvious guess) finds nothing at all.
    """
    row = run(
        storage,
        session,
        "SELECT array_position(ARRAY[1,2,3,2], 2), array_position(ARRAY[1,2,3,2], 2, 3),"
        " array_position(ARRAY[1,2,3,2], 2, 9)",
    ).rows[0]
    assert row == (2, 4, None)


def test_array_position_matches_a_null_to_a_null(storage, session):
    """The SEARCH functions match NULL to NULL. The CONTAINMENT operators do
    not -- see test_sql_array_ops.py. Each looks like the other's bug."""
    row = run(
        storage,
        session,
        "SELECT array_position(ARRAY[1,NULL,2], NULL), array_remove(ARRAY[1,NULL], NULL),"
        " array_replace(ARRAY[1,NULL], NULL, 9)",
    ).rows[0]
    assert row == (2, [1], [1, 9])


def test_array_length_with_a_null_dimension_is_null(storage, session):
    """`int(None)` raised, and the TypeError surfaced as
    `42883 function array_size(integer[], unknown) does not exist` -- a
    signature error naming sqlglot's internal node, for a call PostgreSQL
    simply answers NULL."""
    row = run(
        storage,
        session,
        "SELECT array_length(ARRAY[1,2], NULL), array_length(ARRAY[1,2], 0),"
        " array_length(ARRAY[1,2], 2), array_length(ARRAY[1,2], 1)",
    ).rows[0]
    assert row == (None, None, None, 2)


def test_string_to_array_separator_shapes(storage, session):
    """Three shapes PostgreSQL treats differently. The EMPTY separator reached
    `str.split('')`, which raises -- surfacing as a bogus
    `42883 function string_to_array(unknown, unknown) does not exist` -- and an
    empty INPUT returned `{''}` where PostgreSQL returns `{}`."""
    row = run(
        storage,
        session,
        "SELECT string_to_array('abc', ''), string_to_array('abc', NULL),"
        " string_to_array('', ','), string_to_array('a,b,,c', ','),"
        " string_to_array('a,b', ',', 'b')",
    ).rows[0]
    assert row == (["abc"], ["a", "b", "c"], [], ["a", "b", "", "c"], ["a", None])
