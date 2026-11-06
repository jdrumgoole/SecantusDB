"""The size limits PostgreSQL puts on a table and on a target list, and the
EXPLAIN planning time that follows from a client-sized list (GitHub issues
#2289 and #2346).

PostgreSQL 15.19 refuses a table of more than 1600 columns
(`MaxHeapAttributeNumber`, `DefineRelation`) and a target list of more than
1664 entries (`MaxTupleAttributeNumber`, `transformTargetList` /
`checkTargetlistSize`), both with SQLSTATE 54011 (too_many_columns). The
Rust PG server enforces neither, so one statement can hand EXPLAIN's cost
estimation a list as long as the client likes.

The expected answers are literals measured on PostgreSQL 15.19 (the shared
reference at port 5415, inside a rolled-back transaction).

PostgreSQL has no limit on the number of WHERE conjuncts, so the second item
is about time: planning an EXPLAIN must scale with the number of conjuncts the
client wrote. PostgreSQL 15.19 EXPLAINs a comma join of two tables with
16,000 interleaved conjuncts in 0.24 s, and its time grows 3.7x for 8x the
conjuncts. The Rust server takes 0.9 s for 500 and 5.9 s for 1000 (a debug
build, 2026-11-06).
"""

from __future__ import annotations

import time
from typing import Any

import pytest

from tests.backlog.conftest import pg_answer

# Measured on PostgreSQL 15.19.
TOO_MANY_TABLE_COLUMNS = ("err", "54011", "tables can have at most 1600 columns")
TOO_MANY_TARGETS = ("err", "54011", "target lists can have at most 1664 entries")


@pytest.mark.backlog("RP-387")
def test_create_table_over_1600_columns_is_refused(pg: Any) -> None:
    """`CREATE TABLE` of 1601 columns: PostgreSQL 15.19 answers 54011."""
    cols = ", ".join(f"c{i} int" for i in range(1601))
    try:
        assert pg_answer(pg, f"create table w1601 ({cols})") == TOO_MANY_TABLE_COLUMNS
    finally:
        pg.execute("drop table if exists w1601")


@pytest.mark.backlog("RP-387")
def test_explain_count_target_list_over_1664_entries_is_refused(pg: Any) -> None:
    """The shape of #2289: `EXPLAIN SELECT count(c1), ..., count(cN)`. 1664
    entries plan; 1665 are refused with 54011 -- by EXPLAIN as by SELECT."""
    cols = ", ".join(f"c{i} int" for i in range(1600))
    pg.execute(f"create table w_targets ({cols})")
    try:
        targets = ", ".join(f"count(c{i % 1600})" for i in range(1665))
        assert pg_answer(pg, f"explain select {targets} from w_targets") == TOO_MANY_TARGETS
        plain = ", ".join(f"c{i % 1600}" for i in range(1665))
        assert pg_answer(pg, f"select {plain} from w_targets") == TOO_MANY_TARGETS
    finally:
        pg.execute("drop table w_targets")


@pytest.mark.backlog("RP-387")
def test_explain_distinct_order_by_over_1664_entries_is_refused(pg: Any) -> None:
    """The other shape of #2289: `SELECT DISTINCT <cols> ... ORDER BY <cols>`
    over a list of 1665 entries is refused with 54011."""
    cols = ", ".join(f"c{i} int" for i in range(1600))
    pg.execute(f"create table w_distinct ({cols})")
    try:
        targets = ", ".join(f"c{i % 1600}" for i in range(1665))
        sql = f"explain select distinct {targets} from w_distinct order by {targets}"
        assert pg_answer(pg, sql) == TOO_MANY_TARGETS
    finally:
        pg.execute("drop table w_distinct")


def _interleaved_where(n: int) -> str:
    """The shape of #2346's loop A: a cross-table equality alternating with a
    single-table conjunct, `n` conjuncts in all."""
    return " AND ".join("a.x = b.x" if i % 2 == 0 else f"b.y = {i}" for i in range(n))


def _explain_seconds(pg: Any, n: int) -> float:
    started = time.perf_counter()
    pg.execute(f"explain select * from a, b where {_interleaved_where(n)}")
    return time.perf_counter() - started


@pytest.mark.backlog("RP-388")
def test_explain_comma_join_time_is_linear_in_conjuncts(pg: Any) -> None:
    """EXPLAIN of `FROM a, b WHERE <n interleaved conjuncts>`: four times the
    conjuncts must cost about four times the time. Linear is 4x and quadratic
    16x; a ratio under 8x is generous slack for a loaded machine, and the Rust
    server's is about 40x (n = 250 against n = 1000)."""
    pg.execute("create table a (x int, y int)")
    pg.execute("create table b (x int, y int)")
    try:
        _explain_seconds(pg, 50)  # warm the catalog and plan paths
        small = min(_explain_seconds(pg, 250) for _ in range(2))
        large = _explain_seconds(pg, 1000)
        assert large < 8 * max(small, 0.05), (
            f"EXPLAIN took {small:.2f}s for 250 conjuncts and {large:.2f}s for 1000 "
            f"({large / small:.1f}x for 4x the conjuncts)"
        )
    finally:
        pg.execute("drop table a, b")
