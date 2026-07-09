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
