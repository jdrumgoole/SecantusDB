"""Regression tests of RP-664 (a write re-checked a domain value it did not
coerce into the domain) and of RP-668, and the gap tests of divergences
found while working them:

- PostgreSQL checks a domain where a value is COERCED into it: a column an
  UPDATE does not assign is stored as it was, and a value already of the
  column's domain -- the column itself, through a FROM item, a subquery or
  a WITH; a COALESCE, GREATEST or CASE of it alone; a cast to the domain --
  is no coercion (`coerce_to_target_type` returns it as it is). So a row
  stored before a `NOT VALID` CHECK can still be updated, copied and
  upserted, while every computed or other-typed value is checked;
- an `UPDATE ... FROM` a subquery over the target, whose WHERE names the
  target, was 42P01 (RP-668, fixed): a table named inside a derived table,
  a UNION arm or a nested subquery was taken as bound beside it, so the
  target's name read as the subquery's own;
- RETURNING a FROM item's column, when the join condition reads a column
  the UPDATE assigns, was a named 0A000 (RP-686, fixed but for a subquery
  in RETURNING that reads the FROM item);
- RETURNING read a FROM item after the write, so a FROM item over the
  target saw the rows the statement had changed (UPDATE, RP-937, fixed) or
  deleted (DELETE ... USING, RP-938, fixed but for a subquery in RETURNING
  that reads the USING item);
- a PL/pgSQL assignment to a record's field (`NEW.a := -5`) did not check
  the field's domain, so a trigger that assigned a bad value and then a
  good one was not refused (RP-670, fixed).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t664, s664, p664 cascade",
    "drop domain if exists d664, d664o cascade",
    "create domain d664 as int",
    "create domain d664o as int",
    "create table t664 (a d664, b int, c d664o, n int)",
    "insert into t664 values (-1, -1, -1, null), (5, 5, 5, 1)",
    "create table s664 (k int primary key, a d664)",
    "insert into s664 values (1, -1)",
    "create table p664 (a d664, b int) partition by range (b)",
    "create table p664_1 partition of p664 for values from (-10) to (10)",
    "insert into p664 values (-1, -1)",
    "alter domain d664 add constraint d664_pos check (value > 0) not valid",
    "alter domain d664o add constraint d664o_pos check (value > 0) not valid",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in (
            _SETUP[:2] + _TRIGGER_SETUP[:2] + ["drop function if exists f664t()"] + _SCOPE_SETUP[:1]
        ):
            c.execute(q)


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])
        return cur.fetchall() if cur.description else cur.statusmessage


# Each value is PostgreSQL's answer: `UPDATE 1` / `INSERT 0 n` where the
# write coerces nothing into the domain, 23514 where it does.
WRITES = {
    "update t664 set a = a + 0 where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = a::int where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = a::d664 where b < 0": "UPDATE 1",
    "update t664 set a = c where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = (-1)::d664 where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = coalesce(a, a) where b < 0": "UPDATE 1",
    "update t664 set b = 0, n = n where b < 0": "UPDATE 1",
    "update t664 set n = 2 where b < 0 returning a, n": [(-1, 2)],
    "update t664 x set a = x.a where b < 0": "UPDATE 1",
    "update t664 set (a, b) = (select a, 0 from t664 where b < 0) where b < 0": "UPDATE 1",
    "update t664 set a = t.a from t664 t where t.b < 0 and t664.b < 0": "UPDATE 1",
    "insert into t664 select * from t664 where b < 0": "INSERT 0 1",
    "insert into t664 select a, b, c, 1 from t664 where b < 0": "INSERT 0 1",
    "insert into t664 (a, b) select a, b from t664 where b < 0": "INSERT 0 1",
    (
        "insert into t664 select a, 9, c, 1 from t664 where b < 0 union all "
        "select a, 9, c, 1 from t664 where b < 0"
    ): "INSERT 0 2",
    "insert into t664 select c, 9, a, 1 from t664 where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "insert into t664 select a::int, 9, c, 1 from t664 where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "insert into t664 select x.a, 9, x.c, 1 from t664 x where b < 0": "INSERT 0 1",
    (
        "with q as (select * from t664) insert into t664 select a, 9, c, 1 from q where b < 0"
    ): "INSERT 0 1",
    "insert into s664 values (1, 7) on conflict (k) do update set k = excluded.k": "INSERT 0 1",
    "insert into s664 values (1, 7) on conflict (k) do update set a = s664.a": "INSERT 0 1",
    "insert into s664 values (1, 7) on conflict (k) do update set a = -3": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update p664 set b = 2": "UPDATE 1",
    "update t664 set a = coalesce(a, -1) where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = coalesce(a, '-1') where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = greatest(a, a) where b < 0": "UPDATE 1",
    "update t664 set a = case when b < 0 then a end where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = case when b < 0 then a else a end where b < 0": "UPDATE 1",
    "update t664 set a = (select a from t664 where b < 0) where b > 0": "UPDATE 1",
    "update t664 set a = (select a + 0 from t664 where b < 0) where b > 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = nullif(a, 7) where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set (a, b) = (a, 0) where b < 0": "UPDATE 1",
    "update t664 set (a, b) = (select c, 0 from t664 where b < 0) where b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "update t664 set a = t.c from t664 t where t.b < 0 and t664.b < 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    (
        "insert into t664 select x.a, 9, y.c, 1 from t664 x join t664 y on x.b = y.b where x.b < 0"
    ): "INSERT 0 1",
    "insert into t664 select a, 9, c, 1 from t664 natural join t664 x where b < 0": "INSERT 0 0",
    "insert into t664 select q.a, 9, q.c, 1 from (select * from t664) q where b < 0": "INSERT 0 1",
    (
        "insert into t664 select z, 9, c, 1 from (select a, b, c from t664) q(z, b, c) where b < 0"
    ): "INSERT 0 1",
    (
        "with q(z, y) as (select a, b from t664) insert into t664 select z, 9, "
        "-1::int, 1 from q where y < 0"
    ): (
        "error",
        "23514",
        'value for domain d664o violates check constraint "d664o_pos"',
    ),
    (
        "with q(z, y) as (select a, b from t664) insert into t664 select z, 9, "
        "5, 1 from q where y < 0"
    ): "INSERT 0 1",
    "insert into t664 select a, 9, 5, 1 from t664 where b < 0 union all values (5, 9, 5, 1)": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    (
        "insert into t664 select a, 9, 5, 1 from t664 where b < 0 union all "
        "select c, 9, 5, 1 from t664 where b < 0"
    ): (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    (
        "insert into t664 select a, 9, 5, 1 from t664 where b < 0 union select "
        "a, 9, 5, 1 from t664 where b < 0"
    ): "INSERT 0 1",
    "insert into s664 values (1, 7) on conflict (k) do update set a = excluded.a": "INSERT 0 1",
    "insert into s664 values (1, 7) on conflict (k) do update set a = s664.a + 0": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "insert into s664 as z values (1, 7) on conflict (k) do update set a = z.a": "INSERT 0 1",
    "update p664 set a = a": "UPDATE 1",
    "update p664 set a = -2": (
        "error",
        "23514",
        'value for domain d664 violates check constraint "d664_pos"',
    ),
    "insert into p664 select a, 3 from p664": "INSERT 0 1",
}


@pytest.mark.backlog_fixed("RP-664")
@pytest.mark.parametrize("query", list(WRITES))
def test_write_checks_a_domain_only_where_it_coerces(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == WRITES[query]


# The FROM subquery reads the target table; the outer WHERE names the
# target. The rewrite of an UPDATE ... FROM into correlated subqueries is
# right; the subquery it makes was not seen to read the target, because a
# table named inside a derived table was taken as bound beside it.
FROM_SUBQUERY = {
    "update t664 set a = x.a from (select a from t664 where b < 0) x where t664.b < 0": "UPDATE 1",
    "update t664 set b = x.b from (select b from t664 where b < 0) x where t664.b < 0": "UPDATE 1",
}


@pytest.mark.backlog_fixed("RP-668")
@pytest.mark.parametrize("query", list(FROM_SUBQUERY))
def test_update_from_subquery_over_the_target(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == FROM_SUBQUERY[query]


# RP-668's cause, in a subquery of any statement: a name a derived table, a
# UNION arm or a subquery inside the subquery binds is visible only there
# (PostgreSQL's namespace is per query level), so the same name beside it
# is the enclosing query's. Here every one was 42P01 `missing FROM-clause
# entry for table "t668"`, or -- a name qualified inside the derived table
# -- would have been read as the outer row's.
_SCOPE_SETUP = [
    "drop table if exists t668, u668 cascade",
    "create table t668 (a int, b int)",
    "insert into t668 values (-1, -1), (5, 5), (7, -3)",
    "create table u668 (k int, v int)",
    "insert into u668 values (-1, 10), (5, 50), (7, 70)",
]


def _scoped_answer(dsn: str, query: str) -> object:
    """The statement's answer, and `t668` after it."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SCOPE_SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])
        out = cur.fetchall() if cur.description else cur.statusmessage
        return out, c.execute("select * from t668 order by a, b").fetchall()


_T668 = [(-1, -1), (5, 5), (7, -3)]

# PostgreSQL 15.19's answers, on a private cluster.
SCOPED = {
    "select (select x.a from (select a from t668 where b < 0) x where t668.b < 0 limit 1) v"
    " from t668 order by t668.a": ([(-1,), (None,), (-1,)], _T668),
    "select a, (select count(*) from (select a from t668 where t668.b > 0) x"
    " where t668.b < 0) from t668 order by a": ([(-1, 1), (5, 0), (7, 1)], _T668),
    "select a, (select max(x.a) from (select a from t668 where t668.b < t668.a) x"
    " where x.a <> t668.a) from t668 order by a": ([(-1, 7), (5, 7), (7, None)], _T668),
    "select a, (select sum(x.v) from u668, lateral (select v from u668 i"
    " where i.k = u668.k and t668.a = i.k) x) from t668 order by a": (
        [(-1, 10), (5, 50), (7, 70)],
        _T668,
    ),
    "select a, (with w as (select a from t668 where b > 0) select count(*) from w"
    " where t668.b < 0) from t668 order by a": ([(-1, 1), (5, 0), (7, 1)], _T668),
    "select a, (select count(*) from (select a from t668 union all select k from u668) x"
    " where t668.b < 0) from t668 order by a": ([(-1, 6), (5, 0), (7, 6)], _T668),
    "select a, (select count(*) from (select a from t668 where b < 0 union all"
    " select k from u668 where k > t668.a) x) from t668 order by a": (
        [(-1, 4), (5, 3), (7, 2)],
        _T668,
    ),
    "select a, (select count(*) from (select k from u668 where exists"
    " (select 1 from t668 where t668.a = u668.k)) x where t668.b < 0) from t668 order by a": (
        [(-1, 3), (5, 0), (7, 3)],
        _T668,
    ),
    "select * from t668 where exists (select 1 from (select a from t668 where b < 0) x"
    " where t668.b < 0) order by a": ([(-1, -1), (7, -3)], _T668),
    "update t668 set a = x.v from (select k, v from u668 where v > 20) x"
    " where t668.b < 0 and x.k = t668.a returning t668.a, t668.b": (
        [(70, -3)],
        [(-1, -1), (5, 5), (70, -3)],
    ),
    "delete from t668 using (select a from t668 where b < 0) x where t668.b < 0": (
        "DELETE 2",
        [(5, 5)],
    ),
    "delete from t668 using (select a from t668 where t668.b > 0) x"
    " where t668.a > x.a returning t668.*": ([(7, -3)], [(-1, -1), (5, 5)]),
}


@pytest.mark.backlog_fixed("RP-668")
@pytest.mark.parametrize("query", list(SCOPED))
def test_a_name_bound_inside_a_derived_table_is_not_bound_beside_it(query: str, dsn: str) -> None:
    assert _scoped_answer(dsn, query) == SCOPED[query]


# Found while working RP-668. An UPDATE ... FROM whose join condition reads
# a column the UPDATE assigns, with a RETURNING item over the FROM item:
# PostgreSQL returns the joined row's value; it was a named 0A000, because
# RETURNING found the joining row again over the NEW row (RP-686, fixed:
# the FROM item's values are read from the joined row before the write).
RETURNING_FROM = {
    "update t668 set b = x.b from (select b from t668 where a = 5) x"
    " where t668.b < 0 returning t668.a, x.b": (
        [(-1, 5), (7, 5)],
        [(-1, 5), (5, 5), (7, 5)],
    ),
    "update t668 set b = u668.v from u668 where t668.b < 0 and u668.k = 5"
    " returning t668.a, u668.v": (
        [(-1, 50), (7, 50)],
        [(-1, 50), (5, 5), (7, 50)],
    ),
    # A FROM item's column beside the target's NEW one, in one expression.
    "update t668 set b = u668.v from u668 where t668.b < 0 and u668.k = 5"
    " returning u668.v + t668.b, u668.k * 2 as kk, case when v > 10 then k end": (
        [(100, 10, 5), (100, 10, 5)],
        [(-1, 50), (5, 5), (7, 50)],
    ),
    "update t668 t set b = u.v from u668 u where t.b < 0 and u.k = 5 returning t.a, v": (
        [(-1, 50), (7, 50)],
        [(-1, 50), (5, 5), (7, 50)],
    ),
}


@pytest.mark.backlog_fixed("RP-686")
@pytest.mark.parametrize("query", list(RETURNING_FROM))
def test_returning_a_from_item_when_the_join_reads_an_assigned_column(query: str, dsn: str) -> None:
    assert _scoped_answer(dsn, query) == RETURNING_FROM[query]


# The rest of RP-686: a SUBQUERY in RETURNING that reads a FROM item, when
# the join condition reads an assigned column, is still the named 0A000.
RETURNING_FROM_SUBQUERY = {
    "update t668 set b = u668.v from u668 where t668.b < 0 and u668.k = 5"
    " returning t668.a, (select u668.v)": (
        [(-1, 50), (7, 50)],
        [(-1, 50), (5, 5), (7, 50)],
    ),
}


@pytest.mark.backlog("RP-686")
@pytest.mark.parametrize("query", list(RETURNING_FROM_SUBQUERY))
def test_a_returning_subquery_over_a_from_item_when_the_join_reads_an_assigned_column(
    query: str, dsn: str
) -> None:
    assert _scoped_answer(dsn, query) == RETURNING_FROM_SUBQUERY[query]


# Found while working RP-686. RETURNING read a FROM item (USING item) by
# finding the joining row again AFTER the write, so a FROM item over the
# target saw the rows the statement had just changed or deleted; PostgreSQL
# joins once, before the write, and returns the joined row's values.
RETURNING_FROM_BEFORE_WRITE = {
    "update t668 set b = x.b + 100 from (select a, b from t668) x"
    " where x.a = t668.a returning t668.a, x.b": (
        [(-1, -1), (5, 5), (7, -3)],
        [(-1, 99), (5, 105), (7, 97)],
    ),
    # A self-join: `t668` is the target (the FROM item answers to `s`
    # alone), so `t668.b` is the new value and `s.b` the old one.
    "update t668 set b = s.b + 100 from t668 s where s.a = t668.a"
    " returning t668.a, t668.b, s.b, s.b + t668.b": (
        [(-1, 99, -1, 98), (5, 105, 5, 110), (7, 97, -3, 94)],
        [(-1, 99), (5, 105), (7, 97)],
    ),
}


@pytest.mark.backlog_fixed("RP-937")
@pytest.mark.parametrize("query", list(RETURNING_FROM_BEFORE_WRITE))
def test_update_returning_reads_a_from_item_as_it_was_before_the_write(
    query: str, dsn: str
) -> None:
    assert _scoped_answer(dsn, query) == RETURNING_FROM_BEFORE_WRITE[query]


RETURNING_USING_BEFORE_DELETE = {
    "delete from t668 using (select a, b from t668) x"
    " where x.a = t668.a and t668.b < 0 returning t668.a, x.b": (
        [(-1, -1), (7, -3)],
        [(5, 5)],
    ),
    "delete from t668 using t668 s where s.a = t668.a and t668.b < 0 returning t668.*, s.b": (
        [(-1, -1, -1), (7, -3, -3)],
        [(5, 5)],
    ),
    "delete from t668 t using u668 u where u.k = t.a returning t.a, v, u.v + t.b": (
        [(-1, 10, 9), (5, 50, 55), (7, 70, 67)],
        [],
    ),
}


@pytest.mark.backlog_fixed("RP-938")
@pytest.mark.parametrize("query", list(RETURNING_USING_BEFORE_DELETE))
def test_delete_returning_reads_a_using_item_as_it_was_before_the_delete(
    query: str, dsn: str
) -> None:
    assert _scoped_answer(dsn, query) == RETURNING_USING_BEFORE_DELETE[query]


# The rest of RP-938: a SUBQUERY in RETURNING that reads a USING item still
# finds the joining row after the delete, when a derived table over the
# target no longer holds it.
RETURNING_USING_SUBQUERY = {
    "delete from t668 using (select a, b from t668) x where x.a = t668.a returning (select x.b)": (
        [(-1,), (5,), (-3,)],
        [],
    ),
}


@pytest.mark.backlog("RP-938")
@pytest.mark.parametrize("query", list(RETURNING_USING_SUBQUERY))
def test_a_delete_returning_subquery_reads_a_using_item_as_it_was_before_the_delete(
    query: str, dsn: str
) -> None:
    assert _scoped_answer(dsn, query) == RETURNING_USING_SUBQUERY[query]


# A BEFORE ROW trigger on `t664t`, by the body of its function. PostgreSQL
# coerces (and checks) a value where the row is formed, BEFORE the trigger
# sees it, and a PL/pgSQL assignment to `NEW.a` coerces its own value.
_TRIGGER_SETUP = [
    "drop table if exists t664t cascade",
    "drop domain if exists d664t cascade",
    "create domain d664t as int",
    "create table t664t (a d664t, b int)",
    "insert into t664t values (-1, -1), (5, 5)",
    "alter domain d664t add constraint d664t_pos check (value > 0) not valid",
]
_D664T = ("error", "23514", 'value for domain d664t violates check constraint "d664t_pos"')


def _triggered(dsn: str, body: str, query: str) -> object:
    kind = "insert" if query.startswith("insert") else "update"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _TRIGGER_SETUP:
            c.execute(q)
        c.execute("drop function if exists f664t() cascade")
        c.execute(
            "create function f664t() returns trigger language plpgsql as "
            f"$$ begin {body} return new; end $$"
        )
        c.execute(
            f"create trigger tr664 before {kind} on t664t for each row execute function f664t()"
        )
        try:
            return c.execute(query).statusmessage
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


TRIGGERED = {
    # The trigger leaves `a` alone: the copied value is never coerced.
    ("new.b := new.b + 100;", "insert into t664t select a, b from t664t where b < 0"): (
        "INSERT 0 1"
    ),
    ("new.b := new.b + 100;", "update t664t set b = 0 where b < 0"): "UPDATE 1",
    # The value the statement coerces fails before the trigger replaces it.
    ("new.a := 7;", "insert into t664t values (-7, 1)"): _D664T,
    ("new.a := 7;", "update t664t set a = -7 where b = 5"): _D664T,
    # The value the trigger leaves in the row is checked.
    ("new.a := -5;", "insert into t664t values (7, 1)"): _D664T,
    ("new.a := -5;", "update t664t set b = 0 where b = 5"): _D664T,
}


@pytest.mark.backlog_fixed("RP-664")
@pytest.mark.parametrize("case", list(TRIGGERED))
def test_triggered_write_checks_a_domain_where_it_coerces(case: tuple[str, str], dsn: str) -> None:
    assert _triggered(dsn, *case) == TRIGGERED[case]


# The assignment checks the domain, so the row the trigger finally returns
# (`a` = 7) is never reached. The field was typed by the domain's base and
# the assigned value cast to it, which checked nothing (RP-670).
ASSIGNED_IN_TRIGGER = {
    ("new.a := -5; new.a := 7;", "insert into t664t values (7, 1)"): _D664T,
    ("new.a := -5; new.a := 7;", "update t664t set b = 0 where b = 5"): _D664T,
}


@pytest.mark.backlog_fixed("RP-670")
@pytest.mark.parametrize("case", list(ASSIGNED_IN_TRIGGER))
def test_plpgsql_assignment_to_a_record_field_checks_its_domain(
    case: tuple[str, str], dsn: str
) -> None:
    assert _triggered(dsn, *case) == ASSIGNED_IN_TRIGGER[case]


# What an assignment to a trigger record's domain field coerces, measured on
# a private, freshly initdb'd PostgreSQL 15.19 cluster with `_SETUP_670`
# (RP-670). `exec_assign_value` casts the value from its own type to the
# field's, which is the column's domain (`pg_typeof(new.a)` is it): a value
# already of the domain (`old.a`, a COALESCE of it, a cast to the domain)
# is no coercion and checks nothing, so a row stored before a NOT VALID
# CHECK still passes through; any other is coerced to the base type AT the
# domain's modifier (an overlong `varchar(3)` is 22001 at the assignment,
# not truncated) and checked against every domain of the chain, under the
# name of the domain it is coerced to.
_SETUP_670 = [
    "drop table if exists t670, t670n cascade",
    "drop domain if exists dv670, dd670, d670, d670n cascade",
    "create domain dv670 as varchar(3)",
    "create domain d670 as int check (value > 0)",
    "create domain dd670 as d670 check (value < 100)",
    "create table t670 (w dv670, n dd670, b int)",
    "insert into t670 values ('ab', 5, 5)",
    "create domain d670n as int",
    "create table t670n (a d670n, b int)",
    "insert into t670n values (-1, -1)",
    "alter domain d670n add constraint d670n_pos check (value > 0) not valid",
]
_D670N = ("error", "23514", 'value for domain d670n violates check constraint "d670n_pos"')
_UPDATE_670N = "update t670n set b = 0 where b < 0"
_INSERT_670 = "insert into t670 values ('x', 5, 1)"

FIELD_ASSIGNMENTS = {
    ("t670n", "new.a := old.a;", _UPDATE_670N): "UPDATE 1",
    ("t670n", "new.a := coalesce(old.a, old.a);", _UPDATE_670N): "UPDATE 1",
    ("t670n", "new.a := old.a::d670n;", _UPDATE_670N): "UPDATE 1",
    ("t670n", "new.a := old.a + 0;", _UPDATE_670N): _D670N,
    ("t670n", "new.a := old.a::int;", _UPDATE_670N): _D670N,
    ("t670n", "new.a := null;", _UPDATE_670N): "UPDATE 1",
    ("t670n", "raise exception '%', pg_typeof(new.a);", "insert into t670n values (7, 1)"): (
        "error",
        "P0001",
        "d670n",
    ),
    ("t670", "new.w := 'abcdef'; new.w := 'ab';", _INSERT_670): (
        "error",
        "22001",
        "value too long for type character varying(3)",
    ),
    ("t670", "new.w := 'ab    '; raise exception '[%]', new.w;", _INSERT_670): (
        "error",
        "P0001",
        "[ab ]",
    ),
    ("t670", "new.n := 500; new.n := 5;", _INSERT_670): (
        "error",
        "23514",
        'value for domain dd670 violates check constraint "dd670_check"',
    ),
    ("t670", "new.n := -5; new.n := 5;", _INSERT_670): (
        "error",
        "23514",
        'value for domain dd670 violates check constraint "d670_check"',
    ),
    ("t670", "new.n := '7'; new.b := new.n + 1;", _INSERT_670 + " returning n, b"): [(7, 8)],
}


@pytest.mark.backlog_fixed("RP-670")
@pytest.mark.parametrize("case", list(FIELD_ASSIGNMENTS))
def test_assignment_to_a_domain_field_coerces_only_what_is_not_of_it(
    case: tuple[str, str, str], dsn: str
) -> None:
    table, body, query = case
    kind = "insert" if query.startswith("insert") else "update"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP_670:
            c.execute(q)
        c.execute(
            "create or replace function f670() returns trigger language plpgsql as "
            f"$$ begin {body} return new; end $$"
        )
        c.execute(
            f"create trigger tr670 before {kind} on {table} for each row execute function f670()"
        )
        try:
            cur = c.execute(query)
            got: object = cur.fetchall() if "returning" in query else cur.statusmessage
        except psycopg.Error as e:
            got = ("error", e.sqlstate, str(e).splitlines()[0])
        c.execute("drop table if exists t670, t670n cascade")
        c.execute("drop function if exists f670()")
    assert got == FIELD_ASSIGNMENTS[case]


# A cast of a value already of the domain is no coercion, but one over a
# cast that coerces still checks; a client's parameter declared of the
# domain is checked on its way in (15.19).
DOMAIN_CASTS = {
    "select (-5)::d670::d670": (
        "error",
        "23514",
        'value for domain d670 violates check constraint "d670_check"',
    ),
    "execute p670(-1)": (
        "error",
        "23514",
        'value for domain d670 violates check constraint "d670_check"',
    ),
    "execute p670c(-1)": (
        "error",
        "23514",
        'value for domain d670 violates check constraint "d670_check"',
    ),
    "do $$ declare x d670 := 5; y d670; begin y := x::d670; end $$": "DO",
}


@pytest.mark.backlog_fixed("RP-670")
@pytest.mark.parametrize("query", list(DOMAIN_CASTS))
def test_cast_to_a_domain_of_a_value_already_of_it(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP_670:
            c.execute(q)
        c.execute("prepare p670(d670) as select $1")
        c.execute("prepare p670c(d670) as select $1::d670::d670")
        try:
            cur = c.execute(query)
            got: object = cur.fetchall() if cur.description else cur.statusmessage
        except psycopg.Error as e:
            got = ("error", e.sqlstate, str(e).splitlines()[0])
        c.execute("drop table if exists t670, t670n cascade")
    assert got == DOMAIN_CASTS[query]
