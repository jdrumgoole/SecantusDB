"""Gap tests of the divergences found while working RP-796 (a cast to a
collated domain takes its collation): a `COLLATE` over an expression of a
collated domain is refused, and a VALUES or set-operation column whose rows
carry two different implicit collations sorts bytewise instead of having no
collation.

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
# of the expression's type).
@pytest.mark.backlog("RP-802")
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
# 42P22.
@pytest.mark.backlog("RP-803")
def test_a_column_of_conflicting_implicit_collations_has_none(dsn: str) -> None:  # noqa: F811
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select v from (values ('a'::xu782f), ('B'::xc782f)) s(v) order by v",
                "select v from (select 'a'::xu782f union all select 'B'::xc782f) s(v) order by v",
                "select collation for (v) from (values ('a'::xu782f), ('B'::xc782f)) s(v)",
                "select max(v) from (values ('a'::xu782f), ('B'::xc782f)) s(v)",
            ]
        ]
    assert got == [_INDETERMINATE, _INDETERMINATE, [(None,), (None,)], _INDETERMINATE]
