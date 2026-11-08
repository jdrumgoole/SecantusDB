"""Regression tests of RP-664 (a write re-checked a domain value it did not
coerce into the domain), and the gap test of a divergence found while
working it:

- PostgreSQL checks a domain where a value is COERCED into it: a column an
  UPDATE does not assign is stored as it was, and a value already of the
  column's domain -- the column itself, through a FROM item, a subquery or
  a WITH; a COALESCE, GREATEST or CASE of it alone; a cast to the domain --
  is no coercion (`coerce_to_target_type` returns it as it is). So a row
  stored before a `NOT VALID` CHECK can still be updated, copied and
  upserted, while every computed or other-typed value is checked;
- an `UPDATE ... FROM` a subquery over the target, whose WHERE names the
  target, is 42P01;
- a PL/pgSQL assignment to a record's field (`NEW.a := -5`) does not check
  the field's domain, so a trigger that assigns a bad value and then a good
  one is not refused.

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
        for q in _SETUP[:2] + _TRIGGER_SETUP[:2] + ["drop function if exists f664t()"]:
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
# target. Without the subquery's own WHERE (or with no domain at all) the
# statement runs, so the rewrite of an UPDATE ... FROM into correlated
# subqueries loses the target's name.
FROM_SUBQUERY = {
    "update t664 set a = x.a from (select a from t664 where b < 0) x where t664.b < 0": "UPDATE 1",
    "update t664 set b = x.b from (select b from t664 where b < 0) x where t664.b < 0": "UPDATE 1",
}


@pytest.mark.backlog("RP-668")
@pytest.mark.parametrize("query", list(FROM_SUBQUERY))
def test_update_from_subquery_over_the_target(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == FROM_SUBQUERY[query]


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
# (`a` = 7) is never reached.
ASSIGNED_IN_TRIGGER = {
    ("new.a := -5; new.a := 7;", "insert into t664t values (7, 1)"): _D664T,
    ("new.a := -5; new.a := 7;", "update t664t set b = 0 where b = 5"): _D664T,
}


@pytest.mark.backlog("RP-670")
@pytest.mark.parametrize("case", list(ASSIGNED_IN_TRIGGER))
def test_plpgsql_assignment_to_a_record_field_checks_its_domain(
    case: tuple[str, str], dsn: str
) -> None:
    assert _triggered(dsn, *case) == ASSIGNED_IN_TRIGGER[case]
