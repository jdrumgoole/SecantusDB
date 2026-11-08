"""RP-590's WHERE regression, and the gap test of what was found while
working it: RP-606 (the `record_image` operators `*=` .. `*>=` are
refused).

Two different composite types compare through `record = record`, which
`record_eq` / `record_cmp` run field by field (`test_rp_found_rp530.py`
holds the select-list answers). In a WHERE, a composite column compared
with a record of other field types or names was lowered to a filter that
compared the stored record whole, so it matched no row where PostgreSQL
compares the fields by position or raises `record_eq`'s 42804.

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
    "drop table if exists tca",
    "drop type if exists ca, cb, cc, cd cascade",
    "create type ca as (x int, y text)",
    "create type cb as (x int, y int)",
    "create type cc as (x int, y text, z int)",
    "create type cd as (p int, q text)",
    "create table tca (k int, v ca)",
    "insert into tca values (1, row(1, 'a')), (2, row(2, 'b')), (3, null)",
]

_DISSIMILAR = (
    "42804",
    "cannot compare dissimilar column types text and integer at record column 2",
    None,
    None,
)
_WIDTHS = ("42804", "cannot compare record types with different numbers of columns", None, None)


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


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
        return cur.fetchall() if cur.description else cur.statusmessage


# `record_eq` / `record_cmp` compare by position (`cd`'s names are not
# `ca`'s), check each field pair they reach, and check the widths once the
# pairs both have compared equal (`(2, 'a', 0)::cc` is decided unequal
# before that).
WHERE_COMPOSITES = {
    "select k from tca where v = row(1, 'a')::cd order by k": [(1,)],
    "select k from tca where v < row(2, 'a')::cd order by k": [(1,)],
    "select k from tca where v = (select row(2, 'b')::cd) order by k": [(2,)],
    "select k from tca where v = row(1, 2)::cb order by k": _DISSIMILAR,
    "select k from tca where v = row(2, 2)::cb order by k": _DISSIMILAR,
    "select k from tca where v = row(1, 'a', 0)::cc order by k": _WIDTHS,
    "select k from tca where v = row(2, 'a', 0)::cc order by k": [],
    "select k from tca where v = (1, 'a') order by k": (
        "42804",
        "cannot compare dissimilar column types text and unknown at record column 2",
        None,
        None,
    ),
    "select k from tca where v is not distinct from row(1, 2)::cb order by k": _DISSIMILAR,
    "select k from tca where v in (row(1, 'a', 0)::cc, row(2, 'c', 0)::cc) order by k": _WIDTHS,
    "select k from tca where v not in (row(1, 'a')::cd) order by k": [(2,)],
    "select k from tca where v = any(array[row(1, 2)::cb]) order by k": _DISSIMILAR,
    "select k from tca where v between row(1, 2)::cb and row(1, 3)::cb order by k": _DISSIMILAR,
    "select k from tca where not (v = row(1, 2)::cb) order by k": _DISSIMILAR,
    "select k from tca where v = row(1, 'a')::ca order by k": [(1,)],
    "select k, v = row(2, 'b')::cd from tca order by k": [(1, False), (2, True), (3, None)],
}


@pytest.mark.backlog_fixed("RP-590")
@pytest.mark.parametrize("query", list(WHERE_COMPOSITES))
def test_where_compares_a_composite_column_as_a_record(query: str, dsn: str) -> None:
    """A composite column against another record type in a WHERE is
    `record_eq` / `record_cmp`'s answer. Each was an empty result, or 42883
    `operator does not exist: ca = cb` before RP-590."""
    assert answer(dsn, query) == WHERE_COMPOSITES[query]


# `record_image_eq` and its siblings compare the fields' binary images:
# `1.0` and `1.00` are equal numerics with different images. Between two
# ROW constructors the operator is applied field by field
# (`make_row_comparison_op`), and `integer *= integer` does not exist.
RECORD_IMAGE = {
    "select (select row(1.0)) *= (select row(1.00)), (select row(1.0)) = (select row(1.00))": [
        (False, True)
    ],
    "select (select row(1, 'a'::text)) *< (select row(1, 'b'::text))": [(True,)],
    "select (select row(2)) *<> (select row(2)), (select row(1)) *>= (select row(1))": [
        (False, True)
    ],
    "select row(1, 'a')::ca *= row(1, 'a')::cd": [(True,)],
    "select k, v *= row(2, 'b')::ca from tca order by k": [(1, False), (2, True), (3, None)],
    "select row(1) *= row(1)": (
        "42883",
        "operator does not exist: integer *= integer",
        "No operator matches the given name and argument types. "
        "You might need to add explicit type casts.",
        "15",
    ),
}


@pytest.mark.backlog("RP-606")
@pytest.mark.parametrize("query", list(RECORD_IMAGE))
def test_record_image_operators(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_IMAGE[query]
