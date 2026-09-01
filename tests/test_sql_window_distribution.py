"""``cume_dist()`` and ``percent_rank()``.

Both were `0A000 window function CumeDist is not supported`. Every other window
function measured against PostgreSQL 14.13 (`row_number` / `rank` /
`dense_rank` / `lag` / `lead` / `sum OVER` / `ntile` / `first_value` /
`last_value`) already matched, so these two were the gap.

The part worth pinning is that both are **peer-aware**: rows that tie under the
ORDER BY share a value, which is exactly what makes them different from
`row_number() / n`. A test over distinct values alone would pass with the naive
formula.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    run("CREATE TABLE w (id int, g int, v int)")
    # v ties on rows 1 and 2 — the peer group that the naive formula gets wrong.
    run("INSERT INTO w VALUES (1,1,10),(2,1,10),(3,1,20),(4,2,30)")
    try:
        yield run
    finally:
        storage.close()


def test_cume_dist_counts_the_whole_peer_group(db):
    rows, _ = db("SELECT id, cume_dist() OVER (ORDER BY v) FROM w ORDER BY id")
    # Rows 1 and 2 tie, so both get 2/4 — not 1/4 and 2/4.
    assert rows == [(1, 0.5), (2, 0.5), (3, 0.75), (4, 1.0)]


def test_percent_rank_uses_the_rank_not_the_row_number(db):
    rows, _ = db("SELECT id, percent_rank() OVER (ORDER BY v) FROM w ORDER BY id")
    assert rows == [(1, 0.0), (2, 0.0), (3, 2 / 3), (4, 1.0)]


def test_partitioned(db):
    cume, _ = db("SELECT id, cume_dist() OVER (PARTITION BY g ORDER BY v) FROM w ORDER BY id")
    assert cume == [(1, 2 / 3), (2, 2 / 3), (3, 1.0), (4, 1.0)]
    pct, _ = db("SELECT id, percent_rank() OVER (PARTITION BY g ORDER BY v) FROM w ORDER BY id")
    assert pct == [(1, 0.0), (2, 0.0), (3, 1.0), (4, 0.0)]


def test_percent_rank_of_a_single_row_partition_is_zero(db):
    """The `(rows - 1)` denominator would divide by zero."""
    rows, _ = db("SELECT percent_rank() OVER (PARTITION BY g ORDER BY v) FROM w WHERE g = 2")
    assert rows == [(0.0,)]


def test_no_order_by_makes_every_row_a_peer(db):
    rows, _ = db("SELECT cume_dist() OVER () FROM w")
    assert rows == [(1.0,), (1.0,), (1.0,), (1.0,)]


@pytest.mark.parametrize("fn", ["cume_dist", "percent_rank"])
def test_result_is_double_precision(db, fn):
    _rows, tags = db(f"SELECT {fn}() OVER (ORDER BY v) FROM w")
    assert tags == ["float8"]
