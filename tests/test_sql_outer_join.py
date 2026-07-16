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


# -- trailing RIGHT over a two-table INNER composite (b226) ------------------- #


@pytest.fixture
def trj(storage, session):
    # Self-contained INNER-composite fixture.  `ja⋈jb` on ja.k=jb.ak is the INNER
    # composite; jc sits on the RIGHT of the trailing join.
    #   ja: {k:1}, {k:2}
    #   jb: {id:1, ak:1, bk:10}, {id:2, ak:99, bk:20}  # bk:20 row has NO ja (ak 99)
    #   jc: {cid:100, bk:10}, {cid:200, bk:20}, {cid:300, bk:30}  # bk-keyed (pivot jb)
    #   ka: {ck:100, k:1}, {ck:200, k:2}, {ck:300, k:3}          # k-keyed  (pivot ja)
    storage.q("CREATE TABLE ja (k int primary key)")
    storage.q("INSERT INTO ja VALUES (1), (2)")
    storage.q("CREATE TABLE jb (id int primary key, ak int, bk int)")
    storage.q("INSERT INTO jb VALUES (1, 1, 10), (2, 99, 20)")
    storage.q("CREATE TABLE jc (cid int primary key, bk int)")
    storage.q("INSERT INTO jc VALUES (100, 10), (200, 20), (300, 30)")
    storage.q("CREATE TABLE ka (ck int primary key, k int)")
    storage.q("INSERT INTO ka VALUES (100, 1), (200, 2), (300, 3)")
    return storage


def test_trailing_right_pivot_is_join_table(trj, session):
    # `(ja JOIN jb) RIGHT JOIN jc ON jc.bk = jb.bk`.  Pivot is jb.  Only jb(bk:10)
    # has a ja match, so jc(100) → composite; jc(200) matches jb(bk:20) but that jb
    # has no ja, so the INNER composite is empty → all-NULL pad (NOT jb.bk=20!);
    # jc(300) matches no jb → all-NULL pad.
    rows = q(
        trj,
        session,
        "SELECT jc.cid, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN jc ON jc.bk = jb.bk ORDER BY jc.cid",
    ).rows
    assert rows == [(100, 1, 10), (200, None, None), (300, None, None)]


def test_trailing_right_pivot_is_base_table(trj, session):
    # Same composite, but the RIGHT ON references the *base* (ja): pivot is ja.
    # ja(k:1) has a jb → ka(100) → composite; ka(200) matches ja(k:2) which has no
    # jb → empty composite → NULL; ka(300) matches no ja → NULL.
    rows = q(
        trj,
        session,
        "SELECT ka.ck, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN ka ON ka.k = ja.k ORDER BY ka.ck",
    ).rows
    assert rows == [(100, 1, 10), (200, None, None), (300, None, None)]


def test_trailing_right_all_unmatched_c_preserved(trj, session):
    # Every jc row unmatched → every one preserved with NULL pad.
    trj.q("INSERT INTO jc VALUES (400, 77)")
    rows = q(
        trj,
        session,
        "SELECT jc.cid, jb.bk FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN jc ON jc.bk = jb.bk WHERE jc.cid >= 300 ORDER BY jc.cid",
    ).rows
    assert rows == [(300, None), (400, None)]


def test_trailing_right_multiple_composite_matches(trj, session):
    # A jc row matching more than one composite row yields one output row each.
    trj.q("INSERT INTO jb VALUES (3, 1, 10)")  # a second jb(ak:1, bk:10) under ja(1)
    rows = q(
        trj,
        session,
        "SELECT jc.cid, jb.id FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN jc ON jc.bk = jb.bk ORDER BY jc.cid, jb.id",
    ).rows
    assert rows == [(100, 1), (100, 3), (200, None), (300, None)]


def test_trailing_right_select_star_column_order(trj, session):
    # SELECT * keeps FROM order: ja.k, jb.(id,ak,bk), jc.(cid,bk).
    rows = q(
        trj,
        session,
        "SELECT * FROM ja JOIN jb ON ja.k = jb.ak RIGHT JOIN jc ON jc.bk = jb.bk ORDER BY jc.cid",
    ).rows
    assert rows == [
        (1, 1, 1, 10, 100, 10),
        (None, None, None, None, 200, 20),
        (None, None, None, None, 300, 30),
    ]


def test_trailing_right_with_group_by(trj, session):
    # GROUP BY over the driving (jc) key; count of non-null composite rows.
    rows = q(
        trj,
        session,
        "SELECT jc.cid, count(jb.bk) FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN jc ON jc.bk = jb.bk GROUP BY jc.cid ORDER BY jc.cid",
    ).rows
    assert rows == [(100, 1), (200, 0), (300, 0)]


def test_trailing_right_compound_on(trj, session):
    # A compound RIGHT ON drives the let/pipeline `$lookup` form; the residual
    # jc-only predicate stays sound (pivot is still just jb).
    rows = q(
        trj,
        session,
        "SELECT jc.cid, jb.bk FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN jc ON jc.bk = jb.bk AND jc.cid > 50 ORDER BY jc.cid",
    ).rows
    assert rows == [(100, 10), (200, None), (300, None)]


def test_trailing_right_on_spans_both_composite_tables(trj, session):
    # The RIGHT ON references C and *both* composite tables (jc.bk=jb.bk AND
    # jc.cid=ja.k) — the composite is built forward and o2 filtered generically, so a
    # multi-table o2 is sound. jc(1,10) matches the (ja1, jb bk10) composite row.
    trj.q("INSERT INTO jc VALUES (1, 10)")
    rows = q(
        trj,
        session,
        "SELECT jc.cid, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak "
        "RIGHT JOIN jc ON jc.bk = jb.bk AND jc.cid = ja.k",
    ).rows
    assert _sorted(rows) == _sorted(
        [(1, 1, 10), (100, None, None), (200, None, None), (300, None, None)]
    )


# -- trailing FULL over a two-table INNER composite (b227) ------------------- #


def test_trailing_full_keeps_composite_only_and_unmatched_c(trj, session):
    # Add jb(3, ak:1, bk:99) → composite row (ja1, bk:99) matches NO jc.  FULL keeps
    # it (C NULL) *and* the unmatched jc rows (composite NULL) — the two halves that
    # distinguish FULL from RIGHT.
    trj.q("INSERT INTO jb VALUES (3, 1, 99)")
    rows = q(
        trj,
        session,
        "SELECT jc.cid, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak FULL JOIN jc ON jc.bk = jb.bk",
    ).rows
    assert _sorted(rows) == _sorted(
        [(100, 1, 10), (None, 1, 99), (200, None, None), (300, None, None)]
    )


def test_trailing_full_vs_right_differ_on_composite_only_row(trj, session):
    # Same data: RIGHT drops the composite-only (None, 1, 99) row that FULL keeps.
    trj.q("INSERT INTO jb VALUES (3, 1, 99)")
    right = q(
        trj,
        session,
        "SELECT jc.cid, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak RIGHT JOIN jc ON jc.bk = jb.bk",
    ).rows
    assert _sorted(right) == _sorted([(100, 1, 10), (200, None, None), (300, None, None)])


def test_trailing_full_pivot_is_base_table(trj, session):
    # o2 references the base (ja): pivot is ja.  ka(200) matches ja(2) which has no jb
    # → empty composite → anti row; ka(300) matches no ja → anti row.
    rows = q(
        trj,
        session,
        "SELECT ka.ck, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak FULL JOIN ka ON ka.k = ja.k",
    ).rows
    assert _sorted(rows) == _sorted([(100, 1, 10), (200, None, None), (300, None, None)])


def test_trailing_full_select_star_column_order(trj, session):
    trj.q("INSERT INTO jb VALUES (3, 1, 99)")
    rows = q(
        trj,
        session,
        "SELECT * FROM ja JOIN jb ON ja.k = jb.ak FULL JOIN jc ON jc.bk = jb.bk",
    ).rows
    # ja.k, jb.(id,ak,bk), jc.(cid,bk)
    assert _sorted(rows) == _sorted(
        [
            (1, 1, 1, 10, 100, 10),
            (1, 3, 1, 99, None, None),
            (None, None, None, None, 200, 20),
            (None, None, None, None, 300, 30),
        ]
    )


def test_trailing_full_with_group_by(trj, session):
    trj.q("INSERT INTO jb VALUES (3, 1, 99)")
    rows = q(
        trj,
        session,
        "SELECT jc.cid, count(ja.k) FROM ja JOIN jb ON ja.k = jb.ak "
        "FULL JOIN jc ON jc.bk = jb.bk GROUP BY jc.cid",
    ).rows
    # jc.cid 100→1 composite, NULL→1 (the bk:99 composite-only row), 200→0, 300→0.
    assert _sorted(rows) == _sorted([(100, 1), (None, 1), (200, 0), (300, 0)])


def test_trailing_full_with_where(trj, session):
    # WHERE applies after the union; keep only the unmatched-jc anti rows.
    rows = q(
        trj,
        session,
        "SELECT jc.cid, ja.k FROM ja JOIN jb ON ja.k = jb.ak "
        "FULL JOIN jc ON jc.bk = jb.bk WHERE jc.cid >= 200",
    ).rows
    assert _sorted(rows) == _sorted([(200, None), (300, None)])


def test_trailing_full_on_spans_both_composite_tables(trj, session):
    # Multi-table o2 under FULL. No jc has cid=1, so the (ja1, jb bk10) composite row
    # matches nothing → FULL keeps it as a composite-only row (None on the C side).
    rows = q(
        trj,
        session,
        "SELECT jc.cid, ja.k, jb.bk FROM ja JOIN jb ON ja.k = jb.ak "
        "FULL JOIN jc ON jc.bk = jb.bk AND jc.cid = ja.k",
    ).rows
    assert _sorted(rows) == _sorted(
        [(None, 1, 10), (100, None, None), (200, None, None), (300, None, None)]
    )


# -- trailing RIGHT/FULL over a leading-LEFT composite (b228) ---------------- #


@pytest.fixture
def llj(storage, session):
    # A LEFT JOIN B composite: la⋈lb on la.k=lb.ak, with la(3) having NO lb row so the
    # composite carries the (k:3, NULL) LEFT row that an INNER composite would drop.
    #   la: {k:1}, {k:2}, {k:3}
    #   lb: {id:1, ak:1, bk:10}, {id:2, ak:1, bk:20}, {id:3, ak:2, bk:30}
    #   composite (A LEFT JOIN B): (k1,bk10), (k1,bk20), (k2,bk30), (k3,NULL)
    #   lc: {cid:100, k:1}, {cid:200, k:3}, {cid:300, k:9}   # k-keyed  (pivot la, base)
    #   ld: {cid:100, bk:10}, {cid:200, bk:99}               # bk-keyed (pivot lb)
    storage.q("CREATE TABLE la (k int primary key)")
    storage.q("INSERT INTO la VALUES (1), (2), (3)")
    storage.q("CREATE TABLE lb (id int primary key, ak int, bk int)")
    storage.q("INSERT INTO lb VALUES (1, 1, 10), (2, 1, 20), (3, 2, 30)")
    storage.q("CREATE TABLE lc (cid int primary key, k int)")
    storage.q("INSERT INTO lc VALUES (100, 1), (200, 3), (300, 9)")
    storage.q("CREATE TABLE ld (cid int primary key, bk int)")
    storage.q("INSERT INTO ld VALUES (100, 10), (200, 99)")
    return storage


def test_trailing_right_leftcomposite_pivot_is_base(llj, session):
    # RIGHT ON references the LEFT-preserved base la: the (k:3, NULL) composite row
    # participates, so lc(200, k:3) yields (200, 3, NULL) — NOT the all-NULL pad an
    # INNER composite would give.
    rows = q(
        llj,
        session,
        "SELECT lc.cid, la.k, lb.bk FROM la LEFT JOIN lb ON la.k = lb.ak "
        "RIGHT JOIN lc ON lc.k = la.k",
    ).rows
    assert _sorted(rows) == _sorted([(100, 1, 10), (100, 1, 20), (200, 3, None), (300, None, None)])


def test_trailing_right_leftcomposite_differs_from_inner(llj, session):
    # The same query with an INNER composite drops the b-less la(3), so lc(200) pads
    # all-NULL — the row that distinguishes a LEFT composite from an INNER one.
    rows = q(
        llj,
        session,
        "SELECT lc.cid, la.k, lb.bk FROM la JOIN lb ON la.k = lb.ak RIGHT JOIN lc ON lc.k = la.k",
    ).rows
    assert _sorted(rows) == _sorted(
        [(100, 1, 10), (100, 1, 20), (200, None, None), (300, None, None)]
    )


def test_trailing_right_leftcomposite_pivot_is_join_table(llj, session):
    # RIGHT ON references the non-driving lb: the (a, NULL) rows never satisfy an ON on
    # lb.bk, so this is INNER-equivalent. ld(200, bk:99) matches nothing → NULL pad.
    rows = q(
        llj,
        session,
        "SELECT ld.cid, la.k, lb.bk FROM la LEFT JOIN lb ON la.k = lb.ak "
        "RIGHT JOIN ld ON ld.bk = lb.bk",
    ).rows
    assert _sorted(rows) == _sorted([(100, 1, 10), (200, None, None)])


def test_trailing_full_leftcomposite_pivot_is_base(llj, session):
    # FULL over the LEFT composite: main keeps the composite-only (None, 2, 30) and the
    # b-less (200, 3, NULL); anti keeps the unmatched lc(300).
    rows = q(
        llj,
        session,
        "SELECT lc.cid, la.k, lb.bk FROM la LEFT JOIN lb ON la.k = lb.ak "
        "FULL JOIN lc ON lc.k = la.k",
    ).rows
    assert _sorted(rows) == _sorted(
        [(100, 1, 10), (100, 1, 20), (None, 2, 30), (200, 3, None), (300, None, None)]
    )


def test_trailing_full_leftcomposite_select_star_column_order(llj, session):
    rows = q(
        llj,
        session,
        "SELECT * FROM la LEFT JOIN lb ON la.k = lb.ak FULL JOIN lc ON lc.k = la.k",
    ).rows
    # la.k, lb.(id,ak,bk), lc.(cid,k)
    assert _sorted(rows) == _sorted(
        [
            (1, 1, 1, 10, 100, 1),
            (1, 2, 1, 20, 100, 1),
            (2, 3, 2, 30, None, None),
            (3, None, None, None, 200, 3),
            (None, None, None, None, 300, 9),
        ]
    )


def test_trailing_full_leftcomposite_pivot_is_join_table(llj, session):
    # pivot lb (INNER-equivalent): main emits each composite row LEFT-joined to ld;
    # anti keeps ld(200, bk:99).
    rows = q(
        llj,
        session,
        "SELECT ld.cid, la.k, lb.bk FROM la LEFT JOIN lb ON la.k = lb.ak "
        "FULL JOIN ld ON ld.bk = lb.bk",
    ).rows
    assert _sorted(rows) == _sorted(
        [(100, 1, 10), (None, 1, 20), (None, 2, 30), (None, 3, None), (200, None, None)]
    )


# -- trailing RIGHT/FULL over a THREE-table composite (b229) ----------------- #


@pytest.fixture
def t3(storage, session):
    # 3-table composite ta⋈tb⋈td, joined `ta.ak=tb.bak` then `td.dbk=tb.bk`.
    #   ta: ak 1/2/3        tb: bk 10/20/30/40 with bak 1/2/9/3  (bk30 orphan: bak 9)
    #   td: dk 100/200/300 with dbk 10/20/40
    #   composite (all INNER): R1(ak1,bk10,dk100) R2(ak2,bk20,dk200) R3(ak3,bk40,dk300)
    #   (tb.bk30 drops — bak 9 has no ta; no td has dbk 30.)
    storage.q("CREATE TABLE ta (ak int primary key, av text)")
    storage.q("INSERT INTO ta VALUES (1, 'a1'), (2, 'a2'), (3, 'a3')")
    storage.q("CREATE TABLE tb (bk int primary key, bak int, bv text)")
    storage.q("INSERT INTO tb VALUES (10, 1, 'b1'), (20, 2, 'b2'), (30, 9, 'b3'), (40, 3, 'b4')")
    storage.q("CREATE TABLE td (dk int primary key, dbk int, dv text)")
    storage.q("INSERT INTO td VALUES (100, 10, 'd1'), (200, 20, 'd2'), (300, 40, 'd3')")
    return storage


_T3J = "FROM ta a JOIN tb b ON a.ak = b.bak JOIN td d ON d.dbk = b.bk"


def test_trailing3_right_pivot_is_middle(t3, session):
    # o2 references the middle composite table b. R3 (bk 40) matches no c → dropped
    # (RIGHT); c3 (99) has no composite → all-NULL pad.
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (10, 'c1'), (20, 'c2'), (99, 'c3')")
    rows = q(
        t3, session, "SELECT a.av, b.bk, d.dk, c.cx " + _T3J + " RIGHT JOIN tc c ON c.cx = b.bk"
    ).rows
    assert _sorted(rows) == _sorted(
        [("a1", 10, 100, 10), ("a2", 20, 200, 20), (None, None, None, 99)]
    )


def test_trailing3_full_keeps_composite_only_and_anti(t3, session):
    # FULL adds the composite-only R3 (kept) and the unmatched-c3 anti row.
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (10, 'c1'), (20, 'c2'), (99, 'c3')")
    rows = q(
        t3, session, "SELECT a.av, b.bk, d.dk, c.cx " + _T3J + " FULL JOIN tc c ON c.cx = b.bk"
    ).rows
    assert _sorted(rows) == _sorted(
        [("a1", 10, 100, 10), ("a2", 20, 200, 20), ("a3", 40, 300, None), (None, None, None, 99)]
    )


def test_trailing3_right_no_half_match_leak(t3, session):
    # tb.bk=30 (b3) belongs to NO composite row (its bak 9 has no ta). A c on cx=30
    # must pad the WHOLE composite side NULL — never leak b3's bk/bak/bv. This is the
    # 3-table generalization of the b226 half-match leak.
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (30, 'cBAD')")
    rows = q(
        t3,
        session,
        "SELECT a.av, b.bk, b.bv, d.dk, c.cx " + _T3J + " RIGHT JOIN tc c ON c.cx = b.bk",
    ).rows
    assert rows == [(None, None, None, None, 30)]


def test_trailing3_right_pivot_is_base(t3, session):
    # o2 references the base a.
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (1, 'cA1'), (2, 'cA2'), (7, 'cA9')")
    rows = q(
        t3, session, "SELECT a.av, d.dk, c.cx " + _T3J + " RIGHT JOIN tc c ON c.cx = a.ak"
    ).rows
    assert _sorted(rows) == _sorted([("a1", 100, 1), ("a2", 200, 2), (None, None, 7)])


def test_trailing3_full_pivot_is_last(t3, session):
    # o2 references the last composite table d.
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (100, 'cD1'), (200, 'cD2'), (999, 'cD9')")
    rows = q(t3, session, "SELECT a.av, d.dk, c.cx " + _T3J + " FULL JOIN tc c ON c.cx = d.dk").rows
    assert _sorted(rows) == _sorted(
        [("a1", 100, 100), ("a2", 200, 200), ("a3", 300, None), (None, None, 999)]
    )


def test_trailing3_full_select_star_column_order(t3, session):
    # SELECT * keeps FROM order: a.(ak,av), b.(bk,bak,bv), d.(dk,dbk,dv), c.(cx,cv).
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (100, 'cD1'), (200, 'cD2'), (999, 'cD9')")
    rows = q(t3, session, "SELECT * " + _T3J + " FULL JOIN tc c ON c.cx = d.dk").rows
    assert _sorted(rows) == _sorted(
        [
            (1, "a1", 10, 1, "b1", 100, 10, "d1", 100, "cD1"),
            (2, "a2", 20, 2, "b2", 200, 20, "d2", 200, "cD2"),
            (3, "a3", 40, 3, "b4", 300, 40, "d3", None, None),
            (None, None, None, None, None, None, None, None, 999, "cD9"),
        ]
    )


def test_trailing3_right_with_where_and_group_by(t3, session):
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (10, 'c1'), (20, 'c2'), (99, 'c3')")
    rows = q(
        t3,
        session,
        "SELECT c.cx, count(a.av) " + _T3J + " RIGHT JOIN tc c ON c.cx = b.bk "
        "GROUP BY c.cx HAVING c.cx >= 20 ORDER BY c.cx",
    ).rows
    assert rows == [(20, 1), (99, 0)]


def test_trailing3_right_over_left_composite(t3, session):
    # A LEFT composite: `ta LEFT JOIN tb LEFT JOIN td`. a3 (no tb) survives as
    # (a3, NULL, NULL); a c on a.ak=3 keeps it — an INNER composite would all-NULL pad.
    t3.q("DELETE FROM tb WHERE bk IN (30, 40)")  # a3 now has no tb row
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (1, 'c1'), (3, 'c3'), (9, 'c9')")
    rows = q(
        t3,
        session,
        "SELECT a.av, b.bk, d.dk, c.cx FROM ta a LEFT JOIN tb b ON a.ak = b.bak "
        "LEFT JOIN td d ON d.dbk = b.bk RIGHT JOIN tc c ON c.cx = a.ak",
    ).rows
    assert _sorted(rows) == _sorted(
        [("a1", 10, 100, 1), ("a3", None, None, 3), (None, None, None, 9)]
    )


def _t3_four(t3):
    # Extend the 3-table composite to four: te joins on te.eak = a.ak (one te per a).
    #   composite ta⋈tb⋈td⋈te: R1(a1,e1000) R2(a2,e2000) R3(a3,e3000)
    t3.q("CREATE TABLE te (ek int primary key, eak int, ev text)")
    t3.q("INSERT INTO te VALUES (1000, 1, 'e1'), (2000, 2, 'e2'), (3000, 3, 'e3')")
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    t3.q("INSERT INTO tc VALUES (1000, 'c1'), (2000, 'c2'), (9999, 'c3')")


_T4J = _T3J + " JOIN te e ON e.eak = a.ak"


def test_trailing4_right_composite(t3, session):
    # A four-table composite with a trailing RIGHT. R3 (e3000) matches no tc → dropped.
    _t3_four(t3)
    rows = q(
        t3, session, "SELECT a.av, e.ek, c.cx " + _T4J + " RIGHT JOIN tc c ON c.cx = e.ek"
    ).rows
    assert _sorted(rows) == _sorted([("a1", 1000, 1000), ("a2", 2000, 2000), (None, None, 9999)])


def test_trailing4_full_composite(t3, session):
    # FULL keeps the composite-only R3 (e3000, no tc) and the unmatched tc(9999).
    _t3_four(t3)
    rows = q(t3, session, "SELECT a.av, e.ek, c.cx " + _T4J + " FULL JOIN tc c ON c.cx = e.ek").rows
    assert _sorted(rows) == _sorted(
        [("a1", 1000, 1000), ("a2", 2000, 2000), ("a3", 3000, None), (None, None, 9999)]
    )


def test_trailing_five_table_composite(t3, session):
    # Five tables: prove N-table generality (composite ta⋈tb⋈td⋈te⋈tf, trailing RIGHT).
    _t3_four(t3)
    t3.q("CREATE TABLE tf (fk int primary key, fek int, fv text)")
    t3.q("INSERT INTO tf VALUES (1, 1000, 'f1'), (2, 2000, 'f2'), (3, 3000, 'f3')")
    rows = q(
        t3,
        session,
        "SELECT a.av, f.fk, c.cx " + _T4J + " JOIN tf f ON f.fek = e.ek "
        "RIGHT JOIN tc c ON c.cx = e.ek",
    ).rows
    assert _sorted(rows) == _sorted([("a1", 1, 1000), ("a2", 2, 2000), (None, None, 9999)])


def test_trailing4_still_rejects_bad_shape(t3, session):
    # A non-adjacent composite ON (references a table not yet joined) stays 0A000.
    t3.q("CREATE TABLE te (ek int primary key, eak int, ev text)")
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    with pytest.raises(SQLError) as ei:
        q(
            t3,
            session,
            "SELECT c.cx FROM ta a JOIN tb b ON a.ak = b.bak JOIN te e ON e.eak = d.dk "
            "JOIN td d ON d.dbk = b.bk RIGHT JOIN tc c ON c.cx = b.bk",
        )
    assert ei.value.sqlstate == "0A000"


def test_trailing3_unqualified_on_rejected(t3, session):
    t3.q("CREATE TABLE tc (cx int primary key, cv text)")
    with pytest.raises(SQLError) as ei:
        q(t3, session, "SELECT c.cx " + _T3J + " RIGHT JOIN tc c ON cx = b.bk")
    assert ei.value.sqlstate == "0A000"


# -- leading RIGHT/FULL join + INNER/LEFT tail (b225) ------------------------- #


@pytest.fixture
def lead(storage, session):
    # Self-contained: la(id, av)=(1,a1),(2,a2); lb(id, aid, amt): lb10→a1,
    # lb11→aid 3 (no a); lc(id, bid, cv): lc20→lb10, lc21→bid 99 (no lb).
    storage.q("CREATE TABLE la (id int primary key, av text)")
    storage.q("INSERT INTO la VALUES (1, 'a1'), (2, 'a2')")
    storage.q("CREATE TABLE lb (id int primary key, aid int, amt int)")
    storage.q("INSERT INTO lb VALUES (10, 1, 100), (11, 3, 200)")
    storage.q("CREATE TABLE lc (id int primary key, bid int, cv text)")
    storage.q("INSERT INTO lc VALUES (20, 10, 'c1'), (21, 99, 'cX')")
    return storage


def test_leading_right_then_inner(lead, session):
    # RIGHT keeps lb10(a1) and lb11(no a); INNER JOIN lc keeps only lb10→c1.
    rows = q(
        lead,
        session,
        "SELECT la.av, lb.amt, lc.cv FROM la RIGHT JOIN lb ON la.id = lb.aid "
        "JOIN lc ON lc.bid = lb.id",
    ).rows
    assert _sorted(rows) == [("a1", 100, "c1")]


def test_leading_right_then_left(lead, session):
    rows = q(
        lead,
        session,
        "SELECT la.av, lb.amt, lc.cv FROM la RIGHT JOIN lb ON la.id = lb.aid "
        "LEFT JOIN lc ON lc.bid = lb.id",
    ).rows
    # lb10→c1 ; lb11 (no a) → no c.
    assert _sorted(rows) == [("a1", 100, "c1"), (None, 200, None)]


def test_leading_full_then_inner(lead, session):
    # FULL keeps a2 (no lb) and lb11 (no a); INNER JOIN lc keeps only lb10→c1.
    rows = q(
        lead,
        session,
        "SELECT la.av, lb.amt, lc.cv FROM la FULL JOIN lb ON la.id = lb.aid "
        "JOIN lc ON lc.bid = lb.id",
    ).rows
    assert _sorted(rows) == [("a1", 100, "c1")]


def test_leading_full_then_left_keeps_both_null_sides(lead, session):
    rows = q(
        lead,
        session,
        "SELECT la.av, lb.amt, lc.cv FROM la FULL JOIN lb ON la.id = lb.aid "
        "LEFT JOIN lc ON lc.bid = lb.id",
    ).rows
    # a1/lb10→c1 ; a2 (no lb)→NULL ; lb11 (no a)→NULL.
    assert _sorted(rows) == [("a1", 100, "c1"), ("a2", None, None), (None, 200, None)]


def test_leading_right_tail_with_where(lead, session):
    rows = q(
        lead,
        session,
        "SELECT la.av, lb.amt, lc.cv FROM la RIGHT JOIN lb ON la.id = lb.aid "
        "LEFT JOIN lc ON lc.bid = lb.id WHERE lb.amt >= 200",
    ).rows
    assert _sorted(rows) == [(None, 200, None)]


def test_leading_right_tail_with_group_by(lead, session):
    rows = q(
        lead,
        session,
        "SELECT lb.amt, COUNT(lc.cv) AS n FROM la RIGHT JOIN lb ON la.id = lb.aid "
        "LEFT JOIN lc ON lc.bid = lb.id GROUP BY lb.amt",
    ).rows
    # lb10 (amt 100) has a c match; lb11 (amt 200) has none.
    assert _sorted(rows) == [(100, 1), (200, 0)]


def test_leading_right_tail_select_star_column_order(lead, session):
    res = q(
        lead,
        session,
        "SELECT * FROM la RIGHT JOIN lb ON la.id = lb.aid LEFT JOIN lc ON lc.bid = lb.id",
    )
    # la.*, lb.*, lc.* in FROM order.
    assert [c.name for c in res.columns] == ["id", "av", "id_2", "aid", "amt", "id_3", "bid", "cv"]


def test_double_full_still_rejected(lead, session):
    with pytest.raises(SQLError) as ei:
        q(
            lead,
            session,
            "SELECT * FROM la FULL JOIN lb ON la.id = lb.aid FULL JOIN lc ON lc.bid = lb.id",
        )
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
