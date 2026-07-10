"""Window functions: ``func(...) OVER (PARTITION BY … ORDER BY …)``.

ROW_NUMBER / RANK / DENSE_RANK, the aggregate windows SUM/COUNT/AVG/MIN/MAX
(whole-partition or running under an ORDER BY), and LAG/LEAD. Windows route
through the evaluated-select path: the rows are fetched, each window is computed
over its partitions in Python, and the value is exposed to per-row evaluation.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
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
    s.q("CREATE TABLE sales (id bigint primary key, region text, amount int)")
    rows = [
        (1, "east", 10),
        (2, "east", 30),
        (3, "east", 30),  # tie with id 2 on amount
        (4, "west", 20),
        (5, "west", 50),
    ]
    for i, r, a in rows:
        s.q(f"INSERT INTO sales (id, region, amount) VALUES ({i}, '{r}', {a})")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_row_number_partitioned(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount, id) AS rn "
        "FROM sales ORDER BY id",
    )
    # east by amount,id: 1(10),2(30),3(30) -> rn 1,2,3 ; west: 4(20),5(50) -> 1,2
    assert res.rows == [(1, 1), (2, 2), (3, 3), (4, 1), (5, 2)]
    assert [c.type_tag for c in res.columns] == ["int8", "int8"]


def test_row_number_no_partition(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, ROW_NUMBER() OVER (ORDER BY amount DESC, id) AS rn FROM sales ORDER BY id",
    )
    # window order amount DESC, id: 50(id5)=1, 30(id2)=2, 30(id3)=3, 20(id4)=4, 10(id1)=5
    assert res.rows == [(1, 5), (2, 2), (3, 3), (4, 4), (5, 1)]


def test_rank_and_dense_rank_with_ties(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, RANK() OVER (PARTITION BY region ORDER BY amount) AS r, "
        "DENSE_RANK() OVER (PARTITION BY region ORDER BY amount) AS dr "
        "FROM sales WHERE region = 'east' ORDER BY id",
    )
    # east amounts: id1=10, id2=30, id3=30 -> RANK 1,2,2 ; DENSE_RANK 1,2,2
    assert res.rows == [(1, 1, 1), (2, 2, 2), (3, 2, 2)]


def test_rank_gap_after_tie(storage, session):
    storage.q("CREATE TABLE t (id bigint primary key, v int)")
    for i, v in [(1, 10), (2, 10), (3, 20)]:
        storage.q(f"INSERT INTO t (id, v) VALUES ({i}, {v})")
    res = q(storage, session, "SELECT id, RANK() OVER (ORDER BY v) AS r FROM t ORDER BY id")
    # ties at v=10 -> rank 1,1 then v=20 -> rank 3 (gap)
    assert res.rows == [(1, 1), (2, 1), (3, 3)]


def test_sum_over_whole_partition(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amount) OVER (PARTITION BY region) AS tot FROM sales ORDER BY id",
    )
    # east total = 70 (10+30+30), west total = 70 (20+50)
    assert res.rows == [(1, 70), (2, 70), (3, 70), (4, 70), (5, 70)]


def test_running_sum_with_order(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amount) OVER (PARTITION BY region ORDER BY id) AS run FROM sales "
        "WHERE region = 'west' ORDER BY id",
    )
    # west by id: 20, then 20+50=70
    assert res.rows == [(4, 20), (5, 70)]


def test_running_sum_peers_share_value(storage, session):
    # Default RANGE frame: rows tied on the ORDER BY key share the cumulative
    # value through the end of the peer group.
    res = q(
        storage,
        session,
        "SELECT id, SUM(amount) OVER (PARTITION BY region ORDER BY amount) AS run FROM sales "
        "WHERE region = 'east' ORDER BY id",
    )
    # east by amount: 10 -> 10 ; the two 30s are peers -> 10+30+30 = 70 each
    assert res.rows == [(1, 10), (2, 70), (3, 70)]


def test_avg_over_partition(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, AVG(amount) OVER (PARTITION BY region) AS a FROM sales "
        "WHERE region = 'west' ORDER BY id",
    )
    assert res.rows == [(4, 35.0), (5, 35.0)]


def test_count_over_partition(storage, session):
    res = q(
        storage,
        session,
        "SELECT region, COUNT(*) OVER (PARTITION BY region) AS n FROM sales ORDER BY id",
    )
    assert res.rows == [
        ("east", 3),
        ("east", 3),
        ("east", 3),
        ("west", 2),
        ("west", 2),
    ]


def test_lag_and_lead(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, LAG(amount) OVER (ORDER BY id) AS prev, "
        "LEAD(amount) OVER (ORDER BY id) AS nxt FROM sales ORDER BY id",
    )
    # amounts in id order: 10,30,30,20,50
    assert res.rows == [
        (1, None, 30),
        (2, 10, 30),
        (3, 30, 20),
        (4, 30, 50),
        (5, 20, None),
    ]


def test_lag_offset_and_default(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, LAG(amount, 2, 0) OVER (ORDER BY id) AS prev2 FROM sales ORDER BY id",
    )
    assert res.rows == [(1, 0), (2, 0), (3, 10), (4, 30), (5, 30)]


def test_multiple_windows_one_select(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS rn, "
        "SUM(amount) OVER (PARTITION BY region) AS tot FROM sales ORDER BY id",
    )
    assert res.rows == [(1, 1, 70), (2, 2, 70), (3, 3, 70), (4, 4, 70), (5, 5, 70)]


def test_range_numeric_offset(storage, session):
    # A numeric RANGE offset is a value window on the ORDER BY key. ids 1..5 are
    # dense so each frame is every earlier-or-equal id (key >= cur - 5) — a running
    # sum of amounts 10,30,30,20,50.
    res = q(
        storage,
        session,
        "SELECT id, SUM(amount) OVER (ORDER BY id RANGE BETWEEN 5 PRECEDING AND CURRENT ROW) AS s "
        "FROM sales ORDER BY id",
    )
    assert res.rows == [(1, 10), (2, 40), (3, 70), (4, 90), (5, 140)]
