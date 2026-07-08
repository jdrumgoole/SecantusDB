"""RIGHT and FULL OUTER joins.

`$lookup` is left-driven, so `A RIGHT JOIN B` is planned as `B LEFT JOIN A`
(drive from B, look A up, preserve unmatched B), and `A FULL JOIN B` is the
LEFT join from A unioned with the B rows that found no A match (reshaped so B's
columns sit under its alias and A's read as NULL). Only the two-table case is
supported; a chain mixing in a RIGHT/FULL is rejected.
"""

from __future__ import annotations

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
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE a (id bigint primary key, av text)")
    s.q("CREATE TABLE b (id bigint primary key, aid int, amt int)")
    for i, v in [(1, "a1"), (2, "a2"), (3, "a3")]:
        s.q(f"INSERT INTO a (id, av) VALUES ({i}, '{v}')")
    # b rows 10/11 -> a1, 12 -> a2, 13 -> aid 99 (no a). a3 has no b.
    for i, aid, amt in [(10, 1, 5), (11, 1, 7), (12, 2, 3), (13, 99, 9)]:
        s.q(f"INSERT INTO b (id, aid, amt) VALUES ({i}, {aid}, {amt})")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def _sorted(rows):
    return sorted(rows, key=lambda r: tuple((v is None, v) for v in r))


def test_inner_join_baseline(storage, session):
    rows = q(storage, session, "SELECT a.av, b.amt FROM a JOIN b ON a.id = b.aid").rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), ("a2", 3)]


def test_left_join_baseline(storage, session):
    rows = q(storage, session, "SELECT a.av, b.amt FROM a LEFT JOIN b ON a.id = b.aid").rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), ("a2", 3), ("a3", None)]


def test_right_join_keeps_unmatched_right(storage, session):
    # All b rows kept; b13 (aid 99) has no a -> a columns NULL.
    rows = q(storage, session, "SELECT a.av, b.amt FROM a RIGHT JOIN b ON a.id = b.aid").rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), ("a2", 3), (None, 9)]


def test_right_outer_keyword(storage, session):
    rows = q(storage, session, "SELECT a.av, b.amt FROM a RIGHT OUTER JOIN b ON a.id = b.aid").rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), ("a2", 3), (None, 9)]


def test_full_join_keeps_both_unmatched(storage, session):
    # a3 (no b) and b13 (no a) both survive, padded with NULLs.
    rows = q(storage, session, "SELECT a.av, b.amt FROM a FULL JOIN b ON a.id = b.aid").rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), ("a2", 3), ("a3", None), (None, 9)]


def test_full_outer_keyword(storage, session):
    rows = q(storage, session, "SELECT a.av, b.amt FROM a FULL OUTER JOIN b ON a.id = b.aid").rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), ("a2", 3), ("a3", None), (None, 9)]


def test_right_join_star_column_order(storage, session):
    # SELECT * keeps Postgres left-to-right (FROM-order) column order even though
    # the pipeline drives from the right table.
    res = q(storage, session, "SELECT * FROM a RIGHT JOIN b ON a.id = b.aid")
    assert [c.name for c in res.columns] == ["id", "av", "id_2", "aid", "amt"]
    assert (None, None, 13, 99, 9) in res.rows


def test_right_join_with_where(storage, session):
    # WHERE applies after the join (b.amt > 4 keeps b10/b11/b13).
    rows = q(
        storage, session, "SELECT a.av, b.amt FROM a RIGHT JOIN b ON a.id = b.aid WHERE b.amt > 4"
    ).rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), (None, 9)]


def test_full_join_with_where_filters_null_side(storage, session):
    # b.amt > 4 is false/unknown for the a3-only row (b.amt NULL), so it drops.
    rows = q(
        storage, session, "SELECT a.av, b.amt FROM a FULL JOIN b ON a.id = b.aid WHERE b.amt > 4"
    ).rows
    assert _sorted(rows) == [("a1", 5), ("a1", 7), (None, 9)]


def test_right_join_with_group_by(storage, session):
    rows = q(
        storage,
        session,
        "SELECT b.aid, SUM(b.amt) AS s FROM a RIGHT JOIN b ON a.id = b.aid GROUP BY b.aid",
    ).rows
    assert _sorted(rows) == [(1, 12), (2, 3), (99, 9)]


def test_full_join_with_group_by_counts_unmatched(storage, session):
    # COUNT(b.id) is 0 for a3 (no orders) and counts the unmatched b row under NULL.
    rows = q(
        storage,
        session,
        "SELECT a.av, COUNT(b.id) AS c FROM a FULL JOIN b ON a.id = b.aid GROUP BY a.av",
    ).rows
    assert _sorted(rows) == [("a1", 2), ("a2", 1), ("a3", 0), (None, 1)]


def test_right_join_with_scalar_expr(storage, session):
    # The evaluated-select path (scalar function in the list) over a RIGHT join.
    rows = q(
        storage, session, "SELECT UPPER(a.av) AS u, b.amt FROM a RIGHT JOIN b ON a.id = b.aid"
    ).rows
    assert _sorted(rows) == [("A1", 5), ("A1", 7), ("A2", 3), (None, 9)]


def test_three_table_right_join_rejected(storage, session):
    storage.q("CREATE TABLE c (id bigint primary key)")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT * FROM a RIGHT JOIN b ON a.id = b.aid JOIN c ON c.id = a.id")
    assert ei.value.sqlstate == "0A000"


def test_full_join_compound_on(storage, session):
    # A non-simple ON (compound) drives the let/pipeline lookup form on both arms.
    storage.q("CREATE TABLE x (id bigint primary key, k1 int, k2 int, xv text)")
    storage.q("CREATE TABLE y (id bigint primary key, k1 int, k2 int, yv text)")
    storage.q("INSERT INTO x (id, k1, k2, xv) VALUES (1, 1, 1, 'x11'), (2, 2, 2, 'x22')")
    storage.q("INSERT INTO y (id, k1, k2, yv) VALUES (1, 1, 1, 'y11'), (2, 9, 9, 'y99')")
    rows = q(
        storage,
        session,
        "SELECT x.xv, y.yv FROM x FULL JOIN y ON x.k1 = y.k1 AND x.k2 = y.k2",
    ).rows
    assert _sorted(rows) == [("x11", "y11"), ("x22", None), (None, "y99")]
