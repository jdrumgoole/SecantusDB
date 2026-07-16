"""Arrays of the newer element types + array operators (#123): the ``@>`` /
``<@`` / ``&&`` containment/overlap operators on Postgres arrays, and the array
type OIDs for the recently-added element types (uuid / inet / date / interval /
geometric / bit / xml / ...). (Wire-level array decoding is in
test_pgserver_pg8000.py.)
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql, typemap
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def st(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _fresh(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE a (id int, nums int[])", session=sess)
    run_sql(
        st,
        DB,
        "INSERT INTO a VALUES (1, ARRAY[1,2,3]), (2, ARRAY[4,5]), (3, ARRAY[2,6])",
        session=sess,
    )
    return st, sess


def _run(st, sess, sql):
    return run_sql(st, DB, sql, session=sess)[-1]


def _ids(st, sess, sql):
    return [r[0] for r in _run(st, sess, sql).rows]


# --------------------------------------------------------------------------- #
# Array type OIDs for the new element types
# --------------------------------------------------------------------------- #


def test_new_type_array_oids_registered():
    # Each maps to Postgres' own array-type OID so a driver decodes the elements.
    expected = {
        "uuid[]": 2951,
        "inet[]": 1041,
        "cidr[]": 651,
        "macaddr[]": 1040,
        "date[]": 1182,
        "time[]": 1183,
        "timetz[]": 1270,
        "interval[]": 1187,
        "bit[]": 1561,
        "varbit[]": 1563,
        "money[]": 791,
        "xml[]": 143,
        "point[]": 1017,
        "json[]": 3807,
    }
    for tag, oid in expected.items():
        assert typemap.PG_OID.get(tag) == oid, tag


def test_uuid_array_column_reports_array_oid(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE u (id int, tags uuid[])", session=sess)
    run_sql(
        st,
        DB,
        "INSERT INTO u VALUES (1, ARRAY['11111111-1111-1111-1111-111111111111'::uuid])",
        session=sess,
    )
    r = _run(st, sess, "SELECT tags FROM u")
    assert r.columns[0].type_tag == "uuid[]"
    assert r.columns[0].pg_oid == 2951
    assert r.rows == [(["11111111-1111-1111-1111-111111111111"],)]


# --------------------------------------------------------------------------- #
# Array containment / overlap operators
# --------------------------------------------------------------------------- #


def test_array_contains(st):
    st, sess = _fresh(st)
    assert _ids(st, sess, "SELECT id FROM a WHERE nums @> ARRAY[1,2] ORDER BY id") == [1]
    assert _ids(st, sess, "SELECT id FROM a WHERE nums @> ARRAY[2] ORDER BY id") == [1, 3]
    assert _ids(st, sess, "SELECT id FROM a WHERE nums @> ARRAY[9] ORDER BY id") == []


def test_array_contained_by(st):
    st, sess = _fresh(st)
    assert _ids(st, sess, "SELECT id FROM a WHERE nums <@ ARRAY[1,2,3,4] ORDER BY id") == [1]
    assert _ids(st, sess, "SELECT id FROM a WHERE nums <@ ARRAY[4,5,6] ORDER BY id") == [2]


def test_array_overlaps(st):
    st, sess = _fresh(st)
    assert _ids(st, sess, "SELECT id FROM a WHERE nums && ARRAY[3,4] ORDER BY id") == [1, 2]
    assert _ids(st, sess, "SELECT id FROM a WHERE nums && ARRAY[6] ORDER BY id") == [3]
    assert _ids(st, sess, "SELECT id FROM a WHERE nums && ARRAY[99] ORDER BY id") == []


def test_array_literal_on_left(st):
    st, sess = _fresh(st)
    # ARRAY[...] <@ column — literal on the left side.
    assert _ids(st, sess, "SELECT id FROM a WHERE ARRAY[2] <@ nums ORDER BY id") == [1, 3]


def test_text_array_operators(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, tags text[])", session=sess)
    run_sql(
        st,
        DB,
        "INSERT INTO t VALUES (1, ARRAY['a','b','c']), (2, ARRAY['x','y'])",
        session=sess,
    )
    assert _ids(st, sess, "SELECT id FROM t WHERE tags @> ARRAY['a','b'] ORDER BY id") == [1]
    assert _ids(st, sess, "SELECT id FROM t WHERE tags && ARRAY['y','z'] ORDER BY id") == [2]


def test_array_op_as_computed_boolean(st):
    st, sess = _fresh(st)
    r = _run(st, sess, "SELECT id, nums @> ARRAY[2] AS has2 FROM a ORDER BY id")
    assert r.columns[1].type_tag == "bool"
    assert r.rows == [(1, True), (2, False), (3, True)]


def test_uuid_array_operators(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE u (id int, tags uuid[])", session=sess)
    u1 = "11111111-1111-1111-1111-111111111111"
    u2 = "22222222-2222-2222-2222-222222222222"
    run_sql(st, DB, f"INSERT INTO u VALUES (1, ARRAY['{u1}'::uuid, '{u2}'::uuid])", session=sess)
    run_sql(st, DB, f"INSERT INTO u VALUES (2, ARRAY['{u2}'::uuid])", session=sess)
    got = _ids(st, sess, f"SELECT id FROM u WHERE tags @> ARRAY['{u1}'::uuid] ORDER BY id")
    assert got == [1]


def test_jsonb_containment_still_pushes_down(st):
    # A jsonb (non-array) column keeps the jsonb @> semantics / pushdown path —
    # the array operator handling must not shadow it.
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE j (id int, doc jsonb)", session=sess)
    run_sql(st, DB, """INSERT INTO j VALUES (1, '{"a":1,"b":2}'), (2, '{"a":9}')""", session=sess)
    assert _ids(st, sess, """SELECT id FROM j WHERE doc @> '{"a":1}' ORDER BY id""") == [1]


# --------------------------------------------------------------------------- #
# Index-eligible @> / && lowering (#158)
# --------------------------------------------------------------------------- #


def _idx(st):
    """A table with an index on the array column + a few rows."""
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int primary key, tags text[])", session=sess)
    run_sql(st, DB, "CREATE INDEX ix ON t (tags)", session=sess)
    for i, arr in [(1, "{a,common}"), (2, "{b,common}"), (3, "{c}"), (4, "{a,b}")]:
        run_sql(st, DB, f"INSERT INTO t VALUES ({i}, '{arr}')", session=sess)
    return st, sess


def test_array_contains_single_element(st):
    st, sess = _idx(st)
    assert _ids(st, sess, "SELECT id FROM t WHERE tags @> ARRAY['common'] ORDER BY id") == [1, 2]


def test_array_contains_all_elements(st):
    st, sess = _idx(st)
    assert _ids(st, sess, "SELECT id FROM t WHERE tags @> ARRAY['a','b'] ORDER BY id") == [4]


def test_array_overlaps_indexed(st):
    st, sess = _idx(st)
    assert _ids(st, sess, "SELECT id FROM t WHERE tags && ARRAY['a','c'] ORDER BY id") == [1, 3, 4]


def test_array_contains_single_uses_index(st):
    st, sess = _idx(st)
    plan = _run(st, sess, "EXPLAIN SELECT id FROM t WHERE tags @> ARRAY['common']").rows[0][0]
    assert "Index Scan" in plan


def test_array_overlaps_uses_index(st):
    st, sess = _idx(st)
    plan = _run(st, sess, "EXPLAIN SELECT id FROM t WHERE tags && ARRAY['a']").rows[0][0]
    assert "Index Scan" in plan


def test_array_contains_empty_matches_all(st):
    # arr @> '{}' is true for every row (stays on the per-row path, not $all: []).
    st, sess = _idx(st)
    assert _ids(st, sess, "SELECT id FROM t WHERE tags @> ARRAY[]::text[] ORDER BY id") == [
        1,
        2,
        3,
        4,
    ]


def test_array_contained_by_still_residual(st):
    st, sess = _idx(st)
    assert _ids(st, sess, "SELECT id FROM t WHERE tags <@ ARRAY['a','b','c'] ORDER BY id") == [3, 4]


def test_array_contains_combines_with_and(st):
    st, sess = _idx(st)
    assert _ids(st, sess, "SELECT id FROM t WHERE tags @> ARRAY['a'] AND id < 4 ORDER BY id") == [1]


def test_array_overlaps_int_column(st):
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE n (id int primary key, xs int[])", session=sess)
    for i, arr in [(1, "{1,2}"), (2, "{2,3}"), (3, "{5}")]:
        run_sql(st, DB, f"INSERT INTO n VALUES ({i}, '{arr}')", session=sess)
    assert _ids(st, sess, "SELECT id FROM n WHERE xs && ARRAY[3,5] ORDER BY id") == [2, 3]
    assert _ids(st, sess, "SELECT id FROM n WHERE xs @> ARRAY[2] ORDER BY id") == [1, 2]
