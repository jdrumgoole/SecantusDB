"""Numerics too wide for a Decimal128 group by VALUE, not by stored text.

A `numeric` past 34 significant digits is stored as `{__numeric: <Postgres
text>, __numkey: <scale-free key>}`, so `1e40`, `1e40.0` and `1e40.00` are
three different documents. `$group` compared the documents, so GROUP BY
answered three rows where Postgres answers one, `SELECT DISTINCT` returned
three, and `count(DISTINCT v)` counted three.

Postgres prints the group's FIRST row, text and all: insert `…890.00` first and
GROUP BY answers `…890.00`; insert the bare form first and it answers that
(measured against PostgreSQL 14.24, 2026-09-20). So the value identity is the
grouping key and the first row's value rides along beside it.

Not covered, and still open in `tasks/backlog.md`: a join on a wide key, and
the cross-form case where one row is narrow and an equal one is wide
(`1.5` vs `1.5` written with 39 digits).
"""

from __future__ import annotations

import contextlib
import uuid

import pytest

import pg_oracle
from secantus.sql import engine
from secantus.storage import Storage

W = "1234567890123456789012345678901234567890"  # 40 digits: wider than Decimal128


@pytest.fixture
def run(tmp_path):
    st = Storage(str(tmp_path))

    def q(sql: str):
        return engine.run_sql(st, "postgres", sql)[-1].rows

    try:
        yield q
    finally:
        st.close()


def _seed(q, values: str) -> None:
    q("create table t (id int, v numeric)")
    q(f"insert into t values {values}")


def test_group_by_collapses_scale_variants(run) -> None:
    _seed(run, f"(1, {W}), (2, {W}.0), (3, {W}.00), (4, 1.5)")
    rows = [(str(v), n) for v, n in run("select v, count(*) from t group by v order by v")]
    assert rows == [("1.5", 1), (W, 3)]


def test_group_by_prints_the_first_rows_text(run) -> None:
    """Postgres keeps the scale of the row it saw first."""
    _seed(run, f"(1, {W}.00), (2, {W}.0), (3, {W})")
    assert [str(v) for (v,) in run("select v from t group by v")] == [f"{W}.00"]


def test_distinct_and_count_distinct(run) -> None:
    _seed(run, f"(1, {W}), (2, {W}.0), (3, {W}.00), (4, 1.5), (5, 1.50)")
    assert [str(v) for (v,) in run("select distinct v from t order by v")] == ["1.5", W]
    assert run("select count(distinct v) from t") == [(2,)]


def test_sum_over_the_collapsed_group(run) -> None:
    """The group is one bucket, so the sum covers all three rows -- and keeps
    the widest scale of its inputs, as Postgres does."""
    _seed(run, f"(1, {W}), (2, {W}.0), (3, {W}.00)")
    assert run("select count(*), sum(v)::text from t group by v") == [(3, f"{int(W) * 3}.00")]


def test_grouping_sets_and_rollup(run) -> None:
    run("create table t (id int, g int, v numeric)")
    run(f"insert into t values (1, 1, {W}), (2, 1, {W}.0), (3, 2, {W}.00)")
    rows = run("select v, count(*) from t group by rollup (v) order by 1")
    # One group for the three scale variants, then ROLLUP's total; a NULL key
    # sorts last under Postgres' default ASC.
    assert [(str(v) if v is not None else None, n) for v, n in rows] == [(W, 3), (None, 3)]


def test_a_join_still_groups_by_value(run) -> None:
    run("create table t (id int, v numeric)")
    run("create table u (id int, v numeric)")
    run(f"insert into t values (1, {W}), (2, {W}.0)")
    run("insert into u values (1, 5), (2, 5)")
    rows = run("select t.v, count(*) from t join u on t.id = u.id group by t.v")
    assert [(str(v), n) for v, n in rows] == [(W, 2)]


# ------------------------------------------------------------------ the oracle

_CASES = [
    "select v, count(*) from t group by v order by v",
    "select distinct v from t order by v",
    "select count(distinct v) from t",
    "select v, sum(v) from t group by v order by v",
    "select v, count(*) from t group by rollup (v) order by v nulls last",
]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
@pytest.mark.parametrize("sql", _CASES)
@pytest.mark.parametrize(
    "values",
    [f"(1, {W}), (2, {W}.0), (3, {W}.00), (4, 1.5), (5, 1.50)", f"(1, {W}.00), (2, {W}), (3, 1.5)"],
    ids=["bare-first", "zeros-first"],
)
def test_matches_real_postgres(run, sql: str, values: str) -> None:
    """The same statements against a live PostgreSQL, values compared as text
    so a difference in SCALE is a failure rather than a silent pass."""
    pg = pg_oracle.connect()
    assert pg is not None
    # Every worker shares ONE PostgreSQL, so the table cannot be shared: under
    # `-n auto` two parametrisations ran `drop table t` / `create table t`
    # against each other and CI saw both halves of that race -- `relation "t"
    # does not exist` and a duplicate key on `pg_type_typname_nsp_index`. A
    # schema per test keeps the case SQL (which names a bare `t`) unchanged.
    schema = f"wide_{uuid.uuid4().hex[:12]}"
    try:
        with pg.cursor() as cur:
            cur.execute(f"create schema {schema}")
            cur.execute(f"set search_path to {schema}")
            cur.execute("create table t (id int, v numeric)")
            cur.execute(f"insert into t values {values}")
            cur.execute(sql)
            theirs = [tuple(str(v) for v in row) for row in cur.fetchall()]
        pg.commit()
    finally:
        with contextlib.suppress(Exception), pg.cursor() as cur:
            cur.execute(f"drop schema if exists {schema} cascade")
            pg.commit()
        pg.close()
    _seed(run, values)
    ours = [tuple(str(v) for v in row) for row in run(sql)]
    assert ours == theirs
