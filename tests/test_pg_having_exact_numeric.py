"""HAVING over a numeric sum / min / max compares the exact value.

The select list folds a numeric aggregate in Python (`numeric.fold`), because
Mongo's `$sum` rounds at 34 significant digits and skips a value too wide for a
Decimal128. HAVING compared the in-pipeline accumulator instead, so
``HAVING sum(v) = <the sum the select list returns>`` matched nothing, and a
group whose sum held a 50-digit value compared as if that value were absent.
The statement now takes the per-grouped-row HAVING residual, which sees the
folded value.

Expected rows are what PostgreSQL returns (exact numeric arithmetic).
"""

from __future__ import annotations

import pytest

from secantus.sql import engine
from secantus.storage import Storage

BIG = "12345678901234567890123456789012345678901234567890"


@pytest.fixture
def run(tmp_path):
    st = Storage(str(tmp_path))

    def q(sql: str):
        return engine.run_sql(st, "postgres", sql)[-1].rows

    q("create table t (g int, h int, v numeric)")
    q(
        "insert into t values"
        " (1, 1, 0.1234567890123456789012345678901234567),"
        " (1, 2, 0.0000000000000000000000000000000000001),"
        f" (2, 1, {BIG}), (2, 1, 1),"
        " (3, 2, 1.5), (3, 2, 2.5),"
        " (4, 1, null)"
    )
    try:
        yield q
    finally:
        st.close()


@pytest.mark.parametrize(
    "having,expected",
    [
        # 37 significant digits: Decimal128 rounds the sum to ...4568 or drops it.
        ("sum(v) = 0.1234567890123456789012345678901234568", [(1,)]),
        (f"sum(v) > {BIG}", [(2,)]),
        (f"sum(v) = {BIG[:-1]}1", [(2,)]),
        (f"max(v) > {BIG[:-1]}9 - 10", [(2,)]),
        ("min(v) < 0.00000000000000000000000000000000000011", [(1,)]),
        ("sum(v) = 4", [(3,)]),
        ("sum(v) between 3 and 5", [(3,)]),
        ("sum(v) is null", [(4,)]),
        (f"sum(v) > 1 and count(*) = 2 and sum(v) < {BIG}0", [(2,), (3,)]),
    ],
)
def test_having_sees_the_exact_aggregate(run, having: str, expected) -> None:
    assert run(f"select g from t group by g having {having} order by g") == expected


def test_whole_table_aggregate(run) -> None:
    assert run(f"select count(*) from t having sum(v) > {BIG}") == [(7,)]


def test_with_order_by_and_limit(run) -> None:
    rows = run("select g, sum(v) from t group by g having sum(v) > 1 order by sum(v) desc limit 1")
    assert [(g, str(s)) for g, s in rows] == [(2, BIG[:-1] + "1")]


def test_with_a_window_over_the_groups(run) -> None:
    rows = run(
        "select g, row_number() over (order by g) from t group by g having sum(v) > 1 order by g"
    )
    assert rows == [(2, 1), (3, 2)]
