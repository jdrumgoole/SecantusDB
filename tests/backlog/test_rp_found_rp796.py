"""Gap tests of the divergences found while working RP-796 (a cast to a
collated domain takes its collation): a `COLLATE` over an expression of a
collated domain is refused; a VALUES or set-operation column whose rows
carry two different implicit collations sorted bytewise instead of having no
collation (fixed by RP-803); and such a column is still grouped, made
distinct and compared with itself bytewise, and sorted over no rows without
an error (RP-820).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (locale C.UTF-8, built with ICU) with the `_SETUP`
of `test_rp_found_rp782.py`, which each statement runs after.
"""

from __future__ import annotations

import pytest

from tests.backlog.test_rp_found_rp782 import _answer, _connect, dsn  # noqa: F401

_INDETERMINATE = "42P22 could not determine which collation to use for string comparison"


# `COLLATE` is allowed over any expression of a collatable type, a domain
# over `text` included (`transformCollateClause` asks `type_is_collatable`
# of the expression's type). Here it was 42804; fixed by RP-802.
@pytest.mark.backlog_fixed("RP-802")
def test_collate_over_a_collated_domain_expression(dsn: str) -> None:  # noqa: F811
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select 'a'::xu782f collate \"C\" < 'B'",
                "select v::xu782f collate \"C\" < 'C' from (values ('a'), ('B')) s(v)",
                "select v from (values ('a'), ('B'), ('c'), ('D')) s(v)"
                ' order by v::xu782f collate "C"',
            ]
        ]
    assert got == [
        [(False,)],
        [(False,), (True,)],
        [("B",), ("D",), ("a",), ("c",)],
    ]


# `transformValuesClause` and a set operation take a column's collation by
# `select_common_collation(..., none_ok = true)`: two different implicit
# collations leave the column with none, and sorting or ordering it is then
# 42P22. Here the column had `"default"` and sorted bytewise; fixed by
# RP-803. A sort fails as it starts (`varstr_sortsupport`), a NULL row
# included; `max` / `min` only once they compare two values; the column of
# none, passed through a subquery, stays so and takes no part in a set
# operation's or a comparison's collation.
_V = "(values ('a'::xu782f), ('B'::xc782f)) s(v)"


@pytest.mark.backlog_fixed("RP-803")
def test_a_column_of_conflicting_implicit_collations_has_none(dsn: str) -> None:  # noqa: F811
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                f"select v from {_V} order by v",
                "select v from (select 'a'::xu782f union all select 'B'::xc782f) s(v) order by v",
                f"select collation for (v) from {_V}",
                f"select max(v) from {_V}",
                # More measured with RP-803's fix.
                f"select v from {_V} order by 1",
                "select v from (values ('a'::xu782f), (null::xc782f)) s(v) order by v",
                f"select v from {_V} where v = 'a' order by v",
                f"select max(v) from {_V} where v = 'a'",
                "select min(v) from (values ('a'::xu782f), (null::xc782f)) s(v)",
                f"select max(v) filter (where v = 'B') from {_V}",
                f"select max(v) over () from {_V}",
                f"select max(v) over (rows between current row and current row) from {_V}",
                f"select v < 'b' from {_V}",
                f"select v from (select v from {_V}) t order by v",
                f"select collation for (v) from (select v from {_V}) t",
                "select v from (select c from (select 'a'::xu782f) a(c)"
                " union all select c from (select 'B'::xc782f) b(c)) s(v) order by v",
                f"select v from (select v from {_V} union all select 'z'::text) t order by v",
                f"select v from (select v from {_V} union all select 'Z'::xu782f) t order by v",
                f"select collation for (v) from (select v from {_V}"
                " union all select 'Z'::xu782f) t",
                f'select v from {_V} order by v collate "C"',
            ]
        ]
    assert got == [
        _INDETERMINATE,
        _INDETERMINATE,
        [(None,), (None,)],
        _INDETERMINATE,
        _INDETERMINATE,
        _INDETERMINATE,
        _INDETERMINATE,
        [("a",)],
        [("a",)],
        [("B",)],
        _INDETERMINATE,
        [("a",), ("B",)],
        [(True,), (True,)],
        _INDETERMINATE,
        [(None,), (None,)],
        _INDETERMINATE,
        [("B",), ("a",), ("z",)],
        [("a",), ("B",), ("Z",)],
        [('"und-x-icu"',), ('"und-x-icu"',), ('"und-x-icu"',)],
        [("B",), ("a",)],
    ]


# A column of no collation reaches `check_collation_set` wherever its text is
# hashed or compared: GROUP BY hashes it (`hashtext`), DISTINCT and
# `count(DISTINCT ...)` compare it, as does `v = v` / `v < v` in a select list
# (`texteq`, `text_lt`); a sort fails as it starts, over no rows too. Here
# each was answered bytewise, and the sort over no rows was empty; fixed by
# RP-820.
_HASHING = "42P22 could not determine which collation to use for string hashing"


@pytest.mark.backlog_fixed("RP-820")
def test_a_column_of_no_collation_is_not_hashed_or_compared(dsn: str) -> None:  # noqa: F811
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                f"select count(*) from {_V} group by v",
                f"select distinct v from {_V}",
                f"select count(distinct v) from {_V}",
                f"select v = v from {_V}",
                f"select v < v from {_V}",
                f"select v from {_V} where false order by v",
                # The planner reduces this to `v IS NOT NULL`: nothing compares.
                f"select 1 from {_V} where v = v",
            ]
        ]
    assert got == [
        _HASHING,
        _INDETERMINATE,
        _INDETERMINATE,
        _INDETERMINATE,
        _INDETERMINATE,
        _INDETERMINATE,
        [(1,), (1,)],
    ]
