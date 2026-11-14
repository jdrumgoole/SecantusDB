"""Gap tests of the divergences found while working RP-532 (GROUP BY a whole
row not in the select list): a composite value's null test in a WHERE or
HAVING, ORDER BY a row DESC, an ungrouped whole row's 42803, and a relation
name an alias hides.

An answer is the rows, or `(sqlstate, message, hint, position)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fs, fk cascade",
    "create table fs (a int, c text)",
    "insert into fs values (1, 'y'), (1, 'y'), (2, 'z'), (null, null)",
    "create table fk (k fs)",
    "insert into fk values (row(null, null)), (row(1, null)), (row(1, 'a')), (null)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        return cur.fetchall()


# A composite value IS NULL when every field is, and IS NOT NULL when none
# is (`ExecEvalRowNull`): a stored composite column, a subquery's row
# output, and a grouped whole row alike. A WHERE or HAVING tested whether
# the value itself was null, through a `{x: null}` filter, until RP-534
# evaluated the test per row, as the select list already did.
COMPOSITE_NULL_TEST = {
    "select count(*) from fk where k is not null": [(1,)],
    "select count(*) from fk where k is null": [(2,)],
    "select count(*) from (select fs x from fs) s where x is not null": [(3,)],
    "select count(*) from fs group by fs having fs is null": [(1,)],
    "select count(*) from fs group by fs having fs is not null order by 1": [(1,), (2,)],
}


@pytest.mark.backlog_fixed("RP-534")
@pytest.mark.parametrize("query", list(COMPOSITE_NULL_TEST))
def test_composite_null_test_in_a_clause(query: str, dsn: str) -> None:
    assert answer(dsn, query) == COMPOSITE_NULL_TEST[query]


# A row sorts by `btrecordcmp`, field by field, its NULL fields last: DESC
# reverses that order, the all-NULL row first. Two records had no order at
# all, so a sort kept the input order: `fs` is inserted ascending, which hid
# it for ASC, while `fk`'s rows are not, so its ASC sort pins it too.
ROW_ORDER_DESC = {
    "select fs from fs order by fs desc": [("(,)",), ("(2,z)",), ("(1,y)",), ("(1,y)",)],
    "select count(*) from fs group by row(a, c) order by row(a, c) desc": [
        (1,),
        (1,),
        (2,),
    ],
    "select k from fk order by k": [("(1,a)",), ("(1,)",), ("(,)",), (None,)],
    "select k from fk order by k desc nulls last": [("(,)",), ("(1,)",), ("(1,a)",), (None,)],
}


@pytest.mark.backlog_fixed("RP-535")
@pytest.mark.parametrize("query", list(ROW_ORDER_DESC))
def test_row_order_desc(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ROW_ORDER_DESC[query]


# An ungrouped whole row is a Var of attribute 0, which
# `check_ungrouped_columns` names `fs.*` (`get_rte_attribute_name`), at the
# reference. It was named by its first column, with no position (RP-536).
UNGROUPED_WHOLE_ROW = {
    "select fs, count(*) from fs group by a": (
        "42803",
        'column "fs.*" must appear in the GROUP BY clause or be used in an aggregate function',
        None,
        "8",
    ),
    "select count(*) from fs having fs is not null": (
        "42803",
        'column "fs.*" must appear in the GROUP BY clause or be used in an aggregate function',
        None,
        "32",
    ),
}


@pytest.mark.backlog_fixed("RP-536")
@pytest.mark.parametrize("query", list(UNGROUPED_WHOLE_ROW))
def test_ungrouped_whole_row(query: str, dsn: str) -> None:
    assert answer(dsn, query) == UNGROUPED_WHOLE_ROW[query]


# Under an alias the relation's own name names nothing: a bare `fs` is a
# missing column, at the reference, in every clause.
ALIAS_HIDES_RELATION = {
    "select count(*) from fs x group by fs": (
        "42703",
        'column "fs" does not exist',
        None,
        "36",
    ),
    "select count(*) from fs x order by fs": (
        "42703",
        'column "fs" does not exist',
        None,
        "36",
    ),
}


@pytest.mark.backlog("RP-537")
@pytest.mark.parametrize("query", list(ALIAS_HIDES_RELATION))
def test_alias_hides_relation_name(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ALIAS_HIDES_RELATION[query]
