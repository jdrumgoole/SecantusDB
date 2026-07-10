"""Window polish: explicit frames + value/rank functions.

Explicit ``ROWS`` frames (any ``UNBOUNDED`` / ``CURRENT ROW`` / ``n PRECEDING`` /
``n FOLLOWING`` bound) and ``RANGE`` frames (``UNBOUNDED`` / ``CURRENT ROW``
bounds; a numeric ``RANGE`` offset is rejected), plus the ``NTILE`` /
``FIRST_VALUE`` / ``LAST_VALUE`` / ``NTH_VALUE`` functions. Windows route through
the evaluated-select path; ``secantus.sql.window`` computes the frame per row.
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
    s.q("CREATE TABLE t (id bigint primary key, g text, amt int)")
    rows = [(1, "e", 10), (2, "e", 20), (3, "e", 30), (4, "w", 40), (5, "w", 50)]
    for i, g, a in rows:
        s.q(f"INSERT INTO t (id, g, amt) VALUES ({i}, '{g}', {a})")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


# -- NTILE ------------------------------------------------------------------ #


def test_ntile_two_buckets(storage, session):
    # 5 rows / 2 buckets: the first bucket gets the extra row (3 + 2).
    res = q(storage, session, "SELECT id, NTILE(2) OVER (ORDER BY id) AS nt FROM t ORDER BY id")
    assert res.rows == [(1, 1), (2, 1), (3, 1), (4, 2), (5, 2)]
    assert res.columns[1].type_tag == "int8"


def test_ntile_per_partition(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, NTILE(2) OVER (PARTITION BY g ORDER BY id) AS nt FROM t ORDER BY id",
    )
    # e (3 rows) -> 1,1,2 ; w (2 rows) -> 1,2
    assert res.rows == [(1, 1), (2, 1), (3, 2), (4, 1), (5, 2)]


# -- value functions -------------------------------------------------------- #


def test_first_value(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, FIRST_VALUE(amt) OVER (PARTITION BY g ORDER BY id) AS fv FROM t ORDER BY id",
    )
    assert res.rows == [(1, 10), (2, 10), (3, 10), (4, 40), (5, 40)]


def test_last_value_default_frame_is_current_row(storage, session):
    # The default frame ends at CURRENT ROW, so LAST_VALUE tracks the current row.
    res = q(
        storage,
        session,
        "SELECT id, LAST_VALUE(amt) OVER (PARTITION BY g ORDER BY id) AS lv FROM t ORDER BY id",
    )
    assert res.rows == [(1, 10), (2, 20), (3, 30), (4, 40), (5, 50)]


def test_last_value_whole_partition_frame(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, LAST_VALUE(amt) OVER (PARTITION BY g ORDER BY id "
        "ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS lv FROM t ORDER BY id",
    )
    assert res.rows == [(1, 30), (2, 30), (3, 30), (4, 50), (5, 50)]


def test_nth_value(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, NTH_VALUE(amt, 2) OVER (PARTITION BY g ORDER BY id "
        "ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS nv FROM t ORDER BY id",
    )
    # 2nd value of each partition: e -> 20, w -> 50.
    assert res.rows == [(1, 20), (2, 20), (3, 20), (4, 50), (5, 50)]


def test_nth_value_short_frame_is_null(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, NTH_VALUE(amt, 2) OVER (ORDER BY id) AS nv FROM t ORDER BY id",
    )
    # default frame = UNBOUNDED PRECEDING..CURRENT ROW; row 1's frame has 1 row.
    assert res.rows == [(1, None), (2, 20), (3, 20), (4, 20), (5, 20)]


# -- ROWS frames ------------------------------------------------------------ #


def test_rows_preceding_and_following(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS s "
        "FROM t ORDER BY id",
    )
    # sliding 3-row window (clamped at the ends).
    assert res.rows == [(1, 30), (2, 60), (3, 90), (4, 120), (5, 90)]


def test_rows_running_sum(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) "
        "AS s FROM t ORDER BY id",
    )
    assert res.rows == [(1, 10), (2, 30), (3, 60), (4, 100), (5, 150)]


def test_rows_current_to_unbounded_following(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING) "
        "AS s FROM t ORDER BY id",
    )
    assert res.rows == [(1, 150), (2, 140), (3, 120), (4, 90), (5, 50)]


def test_rows_only_start_runs_to_current_row(storage, session):
    # ``ROWS n PRECEDING`` (start only) frames from n preceding through CURRENT ROW.
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (ORDER BY id ROWS 1 PRECEDING) AS s FROM t ORDER BY id",
    )
    assert res.rows == [(1, 10), (2, 30), (3, 50), (4, 70), (5, 90)]


def test_rows_following_only_window(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, COUNT(*) OVER (ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 2 FOLLOWING) AS c "
        "FROM t ORDER BY id",
    )
    assert res.rows == [(1, 2), (2, 2), (3, 2), (4, 1), (5, 0)]


# -- RANGE frames ----------------------------------------------------------- #


def test_range_whole_partition(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (PARTITION BY g ORDER BY id "
        "RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS s FROM t ORDER BY id",
    )
    assert res.rows == [(1, 60), (2, 60), (3, 60), (4, 90), (5, 90)]


def test_range_peers_share_value(storage, session):
    # Two rows tied on the ORDER BY key share the cumulative RANGE value.
    storage.q("CREATE TABLE p (id bigint primary key, k int, v int)")
    for i, k, v in [(1, 1, 10), (2, 2, 20), (3, 2, 30)]:
        storage.q(f"INSERT INTO p (id, k, v) VALUES ({i}, {k}, {v})")
    res = q(
        storage,
        session,
        "SELECT id, SUM(v) OVER "
        "(ORDER BY k RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS s "
        "FROM p ORDER BY id",
    )
    # k=1 -> 10 ; the two k=2 peers -> 10+20+30 = 60 each.
    assert res.rows == [(1, 10), (2, 60), (3, 60)]


def test_range_numeric_offset_preceding(storage, session):
    # A row is in-frame when its key is within 5 of the current key (ids 1..5 are
    # dense, so each frame is every earlier-or-equal id whose key >= cur - 5).
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (ORDER BY id RANGE BETWEEN 5 PRECEDING AND CURRENT ROW) AS s "
        "FROM t ORDER BY id",
    )
    assert res.rows == [(1, 10), (2, 30), (3, 60), (4, 100), (5, 150)]


def test_range_numeric_offset_distance_not_rowcount(storage, session):
    # Gapped keys: the frame is a value window, not a row count. Keys 1,2,5,6,10.
    storage.q("CREATE TABLE p (k bigint primary key, v int)")
    for k, v in [(1, 1), (2, 1), (5, 1), (6, 1), (10, 1)]:
        storage.q(f"INSERT INTO p (k, v) VALUES ({k}, {v})")
    res = q(
        storage,
        session,
        "SELECT k, SUM(v) OVER (ORDER BY k RANGE BETWEEN 2 PRECEDING AND 2 FOLLOWING) AS s "
        "FROM p ORDER BY k",
    )
    # k1:{1,2} k2:{1,2} k5:{5,6} k6:{5,6} k10:{10}
    assert res.rows == [(1, 2), (2, 2), (5, 2), (6, 2), (10, 1)]


def test_range_numeric_offset_desc(storage, session):
    # DESC flips the inequality: PRECEDING covers higher keys (earlier in the DESC
    # walk). id1's frame is every id with key in [1, 6] = all rows.
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER "
        "(ORDER BY id DESC RANGE BETWEEN 5 PRECEDING AND CURRENT ROW) AS s "
        "FROM t ORDER BY id",
    )
    assert res.rows == [(1, 150), (2, 140), (3, 120), (4, 90), (5, 50)]


def test_range_numeric_offset_following(storage, session):
    res = q(
        storage,
        session,
        "SELECT id, SUM(amt) OVER (ORDER BY id RANGE BETWEEN CURRENT ROW AND 2 FOLLOWING) AS s "
        "FROM t ORDER BY id",
    )
    # id1:{1,2,3}=60 id2:{2,3,4}=90 id3:{3,4,5}=120 id4:{4,5}=90 id5:{5}=50
    assert res.rows == [(1, 60), (2, 90), (3, 120), (4, 90), (5, 50)]


def test_range_numeric_offset_multi_order_key_rejected(storage, session):
    # Postgres requires exactly one ORDER BY column for an offset RANGE frame.
    with pytest.raises(SQLError) as ei:
        q(
            storage,
            session,
            "SELECT SUM(amt) OVER "
            "(ORDER BY g, id RANGE BETWEEN 5 PRECEDING AND CURRENT ROW) FROM t",
        )
    assert ei.value.sqlstate == "0A000"
