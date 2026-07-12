"""Set operations: UNION / UNION ALL / INTERSECT / EXCEPT (and ALL variants).

Each arm runs through the full SELECT path; the rows are combined with the
operation's multiset semantics. Output column names come from the first arm
(Postgres' rule), and a trailing ORDER BY / LIMIT applies to the whole result.
"""

from __future__ import annotations

import bson
import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.insert(
        DB,
        "a",
        [
            {"_id": bson.Int64(1), "n": bson.Int64(1)},
            {"_id": bson.Int64(2), "n": bson.Int64(2)},
            {"_id": bson.Int64(3), "n": bson.Int64(2)},  # duplicate value 2
            {"_id": bson.Int64(4), "n": bson.Int64(3)},
        ],
    )
    s.insert(
        DB,
        "b",
        [
            {"_id": bson.Int64(1), "n": bson.Int64(2)},
            {"_id": bson.Int64(2), "n": bson.Int64(3)},
            {"_id": bson.Int64(3), "n": bson.Int64(4)},
        ],
    )
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0].rows


def col_names(storage, session, sql):
    return [c.name for c in run_sql(storage, DB, sql, session=session)[0].columns]


def vals(storage, session, sql):
    return sorted(r[0] for r in rows(storage, session, sql))


def test_union_dedups(storage, session):
    # a.n = {1,2,2,3}, b.n = {2,3,4}; UNION dedups → {1,2,3,4}.
    assert vals(storage, session, "SELECT n FROM a UNION SELECT n FROM b") == [1, 2, 3, 4]


def test_union_all_keeps_duplicates(storage, session):
    # 4 rows from a + 3 from b, no dedup.
    assert vals(storage, session, "SELECT n FROM a UNION ALL SELECT n FROM b") == [
        1,
        2,
        2,
        2,
        3,
        3,
        4,
    ]


def test_intersect(storage, session):
    # values in both a and b: {2,3}.
    assert vals(storage, session, "SELECT n FROM a INTERSECT SELECT n FROM b") == [2, 3]


def test_intersect_all_multiplicity(storage, session):
    # a has two 2s, b has one 2 → INTERSECT ALL yields min(2,1)=1 copy of 2,
    # and min(1,1)=1 copy of 3.
    assert vals(storage, session, "SELECT n FROM a INTERSECT ALL SELECT n FROM b") == [2, 3]


def test_except(storage, session):
    # a values not in b: a={1,2,3}, b={2,3,4} → {1}.
    assert vals(storage, session, "SELECT n FROM a EXCEPT SELECT n FROM b") == [1]


def test_except_all_multiplicity(storage, session):
    # a multiset {1,2,2,3} minus b multiset {2,3,4}: remove one 2, one 3 → {1,2}.
    assert vals(storage, session, "SELECT n FROM a EXCEPT ALL SELECT n FROM b") == [1, 2]


def test_union_column_names_from_first_arm(storage, session):
    # Output column name comes from the first SELECT, even if the arms differ.
    assert col_names(storage, session, "SELECT n AS x FROM a UNION SELECT n AS y FROM b") == ["x"]


def test_union_order_by_and_limit(storage, session):
    res = rows(storage, session, "SELECT n FROM a UNION SELECT n FROM b ORDER BY n DESC LIMIT 2")
    assert res == [(4,), (3,)]


def test_union_order_by_ordinal(storage, session):
    res = rows(storage, session, "SELECT n FROM a UNION SELECT n FROM b ORDER BY 1")
    assert res == [(1,), (2,), (3,), (4,)]


def test_chained_union(session, tmp_path):
    s = Storage(str(tmp_path / "chain"))
    try:
        s.insert(DB, "x", [{"_id": bson.Int64(1), "n": bson.Int64(1)}])
        s.insert(DB, "y", [{"_id": bson.Int64(1), "n": bson.Int64(2)}])
        s.insert(DB, "z", [{"_id": bson.Int64(1), "n": bson.Int64(3)}])
        assert vals(s, session, "SELECT n FROM x UNION SELECT n FROM y UNION SELECT n FROM z") == [
            1,
            2,
            3,
        ]
    finally:
        s.close()


def test_union_multi_column(session, tmp_path):
    s = Storage(str(tmp_path / "multi"))
    try:
        s.insert(DB, "p", [{"_id": bson.Int64(1), "g": "x", "n": bson.Int64(1)}])
        s.insert(DB, "q", [{"_id": bson.Int64(1), "g": "x", "n": bson.Int64(1)}])
        # Identical (g, n) rows collapse under UNION.
        assert rows(s, session, "SELECT g, n FROM p UNION SELECT g, n FROM q") == [("x", 1)]
    finally:
        s.close()


def test_arity_mismatch_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        rows(storage, session, "SELECT n FROM a UNION SELECT _id, n FROM b")
    assert ei.value.sqlstate == "42601"


# -- VALUES lists — standalone and as a set-operation arm (b232) -------------- #


def test_values_standalone(storage, session):
    res = run_sql(storage, DB, "VALUES (1, 'a'), (2, 'b')", session=session)[0]
    assert res.rows == [(1, "a"), (2, "b")]
    assert [c.name for c in res.columns] == ["column1", "column2"]
    assert [c.type_tag for c in res.columns] == ["int4", "text"]


def test_values_order_by_and_limit(storage, session):
    assert rows(storage, session, "VALUES (3), (1), (2) ORDER BY 1 DESC LIMIT 2") == [(3,), (2,)]


def test_values_order_by_output_column_name(storage, session):
    assert rows(storage, session, "VALUES (3), (1), (2) ORDER BY column1") == [(1,), (2,), (3,)]


def test_values_computed_cells(storage, session):
    # Cells are constant expressions, evaluated with no row scope.
    assert rows(storage, session, "VALUES (1 + 1, upper('x'))") == [(2, "X")]


def test_values_as_union_right_arm(storage, session):
    assert rows(storage, session, "SELECT n FROM a UNION VALUES (7), (8) ORDER BY 1") == [
        (1,),
        (2,),
        (3,),
        (7,),
        (8,),
    ]


def test_values_as_union_left_arm_names_columns(storage, session):
    # First arm (VALUES) supplies the output column name → column1.
    res = run_sql(storage, DB, "VALUES (7), (8) UNION SELECT n FROM a ORDER BY 1", session=session)[
        0
    ]
    assert [c.name for c in res.columns] == ["column1"]
    assert res.rows == [(1,), (2,), (3,), (7,), (8,)]


def test_values_union_values(storage, session):
    assert rows(storage, session, "VALUES (1), (2) UNION VALUES (2), (3) ORDER BY 1") == [
        (1,),
        (2,),
        (3,),
    ]


def test_values_except(storage, session):
    assert rows(storage, session, "SELECT n FROM a EXCEPT VALUES (2), (3) ORDER BY 1") == [(1,)]


def test_values_uneven_width_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        rows(storage, session, "VALUES (1, 2), (3)")
    assert ei.value.sqlstate == "42601"


def test_values_arm_arity_mismatch_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        rows(storage, session, "SELECT n FROM a UNION VALUES (1, 2)")
    assert ei.value.sqlstate == "42601"


def test_values_expression_order_by_rejected(storage, session):
    # Postgres allows only an ordinal / output-column ORDER BY on a VALUES list.
    with pytest.raises(SQLError) as ei:
        rows(storage, session, "VALUES (1), (2) ORDER BY column1 + 1")
    assert ei.value.sqlstate == "42703"
