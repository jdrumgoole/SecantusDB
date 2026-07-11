"""RIGHT and FULL OUTER joins.

`$lookup` is left-driven, so `A RIGHT JOIN B` is planned as `B LEFT JOIN A`
(drive from B, look A up, preserve unmatched B), and `A FULL JOIN B` is the
LEFT join from A unioned with the B rows that found no A match (reshaped so B's
columns sit under its alias and A's read as NULL). A *pure*-RIGHT chain of 3+
tables reverses into a LEFT chain driven from the last table (adjacency-guarded);
mixed LEFT/RIGHT chains, a non-adjacent RIGHT ON, and any multi-table FULL stay
`0A000`.
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


def test_mixed_inner_and_right_chain_rejected(storage, session):
    # A chain mixing an INNER (or LEFT) join with a RIGHT one can't be reversed into
    # a single pure-LEFT chain — it stays 0A000.
    storage.q("CREATE TABLE c (id bigint primary key)")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT * FROM a RIGHT JOIN b ON a.id = b.aid JOIN c ON c.id = a.id")
    assert ei.value.sqlstate == "0A000"


# -- pure-RIGHT chains of 3+ tables ------------------------------------------ #


@pytest.fixture
def chain(storage, session):
    # a(id, av), b(id, aid, bv), c(id, bid, cv): each RIGHT-joins the prior table.
    storage.q("CREATE TABLE bb (id bigint primary key, aid bigint, bv text)")
    storage.q("CREATE TABLE cc (id bigint primary key, bid bigint, cv text)")
    storage.q("INSERT INTO bb (id, aid, bv) VALUES (10, 1, 'b10'), (11, 1, 'b11'), (12, 99, 'b12')")
    storage.q(
        "INSERT INTO cc (id, bid, cv) VALUES "
        "(100, 10, 'c100'), (101, 12, 'c101'), (102, 88, 'c102')"
    )
    return storage


def test_three_table_right_chain(chain, session):
    # Keep every cc row, then match bb, then aa. c100→b10→a1 ; c101→b12(aid 99, no a)
    # ; c102→(bid 88, no b, so no a).
    rows = q(
        chain,
        session,
        "SELECT a.av, bb.bv, cc.cv FROM a RIGHT JOIN bb ON a.id = bb.aid "
        "RIGHT JOIN cc ON bb.id = cc.bid",
    ).rows
    assert _sorted(rows) == [("a1", "b10", "c100"), (None, "b12", "c101"), (None, None, "c102")]


def test_three_table_right_chain_where(chain, session):
    rows = q(
        chain,
        session,
        "SELECT a.av, cc.cv FROM a RIGHT JOIN bb ON a.id = bb.aid "
        "RIGHT JOIN cc ON bb.id = cc.bid WHERE cc.cv IS NOT NULL ORDER BY cc.cv",
    ).rows
    assert rows == [("a1", "c100"), (None, "c101"), (None, "c102")]


def test_three_table_right_chain_select_star_column_order(chain, session):
    # SELECT * keeps FROM order (a.*, bb.*, cc.*) even though the pipeline drives
    # from cc; the fully-unmatched cc row null-pads a and bb.
    res = q(
        chain,
        session,
        "SELECT * FROM a RIGHT JOIN bb ON a.id = bb.aid RIGHT JOIN cc ON bb.id = cc.bid "
        "ORDER BY cc.id",
    )
    assert [c.name for c in res.columns] == ["id", "av", "id_2", "aid", "bv", "id_3", "bid", "cv"]
    assert res.rows[-1] == (None, None, None, None, None, 102, 88, "c102")


def test_three_table_right_chain_group_by(chain, session):
    rows = q(
        chain,
        session,
        "SELECT cc.cv, count(a.av) FROM a RIGHT JOIN bb ON a.id = bb.aid "
        "RIGHT JOIN cc ON bb.id = cc.bid GROUP BY cc.cv ORDER BY cc.cv",
    ).rows
    # only c100 has a matching a; count(a.av) counts non-null → 1, else 0.
    assert rows == [("c100", 1), ("c101", 0), ("c102", 0)]


def test_four_table_right_chain(chain, session):
    chain.q("CREATE TABLE dd (id bigint primary key, cid bigint, dv text)")
    chain.q("INSERT INTO dd (id, cid, dv) VALUES (1000, 100, 'd1000'), (1001, 999, 'd1001')")
    rows = q(
        chain,
        session,
        "SELECT a.av, bb.bv, cc.cv, dd.dv FROM a RIGHT JOIN bb ON a.id = bb.aid "
        "RIGHT JOIN cc ON bb.id = cc.bid RIGHT JOIN dd ON cc.id = dd.cid ORDER BY dd.dv",
    ).rows
    assert rows == [("a1", "b10", "c100", "d1000"), (None, None, None, "d1001")]


def test_right_chain_non_adjacent_on_rejected(chain, session):
    # The 2nd RIGHT ON references a non-adjacent table (a, not bb) — can't reverse.
    with pytest.raises(SQLError) as ei:
        q(
            chain,
            session,
            "SELECT * FROM a RIGHT JOIN bb ON a.id = bb.aid RIGHT JOIN cc ON a.id = cc.bid",
        )
    assert ei.value.sqlstate == "0A000"


def test_mixed_left_right_chain_rejected(chain, session):
    with pytest.raises(SQLError) as ei:
        q(
            chain,
            session,
            "SELECT * FROM a LEFT JOIN bb ON a.id = bb.aid RIGHT JOIN cc ON bb.id = cc.bid",
        )
    assert ei.value.sqlstate == "0A000"


def test_full_in_three_table_chain_rejected(chain, session):
    with pytest.raises(SQLError) as ei:
        q(
            chain,
            session,
            "SELECT * FROM a FULL JOIN bb ON a.id = bb.aid RIGHT JOIN cc ON bb.id = cc.bid",
        )
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
