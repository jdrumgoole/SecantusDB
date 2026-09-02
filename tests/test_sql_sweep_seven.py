"""Seventh differential sweep against PostgreSQL 14.13.

char(n) blank-insensitive comparison, UNNEST ... WITH ORDINALITY column
aliases, greatest/least result typing, and num_nulls/num_nonnulls. Every
expectation here was measured against the reference server, not derived from
this engine's own output.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "s7"))
    try:
        yield s
    finally:
        s.close()


def _rows(store, sql, session):
    return [r for r in run_sql(store, "t", sql, session=session)][0].rows


@pytest.fixture
def sess(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE s7 (id int PRIMARY KEY, c char(5), v varchar(5))", s)
    _rows(store, "INSERT INTO s7 VALUES (1,'ab','ab'),(2,'cd','cd')", s)
    return s


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # PG strips trailing blanks from BOTH operands of a bpchar comparison,
        # so every one of these matches the char(5) holding 'ab'.
        ("SELECT c = 'ab', c = 'ab   ', c = 'ab  x' FROM s7 WHERE id=1", [(True, True, False)]),
        ("SELECT c <> 'ab   ' FROM s7 WHERE id=1", [(False,)]),
        ("SELECT id FROM s7 WHERE c = 'ab   '", [(1,)]),
        ("SELECT id FROM s7 WHERE 'ab  ' = c", [(1,)]),
        ("SELECT id FROM s7 WHERE c IN ('ab   ','zz')", [(1,)]),
        ("SELECT id FROM s7 WHERE c <= 'ab   '", [(1,)]),
        # varchar really is blank-sensitive — the same trim must NOT apply.
        ("SELECT id FROM s7 WHERE v = 'ab   '", []),
        ("SELECT id FROM s7 WHERE v = 'ab'", [(1,)]),
        # The stored value stays unpadded, so length/|| see the bare string.
        ("SELECT c || '|', length(c) FROM s7 WHERE id=1", [("ab|", 2)]),
    ],
)
def test_bpchar_comparison_ignores_trailing_blanks(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # sqlglot hoists an UNNEST's last alias column into `offset`; putting it
        # back is what names the ordinality column `i` rather than `ordinality`.
        (
            "SELECT * FROM unnest(ARRAY['a','b']) WITH ORDINALITY AS t(v, i) ORDER BY i",
            [("a", 1), ("b", 2)],
        ),
        (
            "SELECT i, v FROM unnest(ARRAY['a','b']) WITH ORDINALITY AS t(v, i)",
            [(1, "a"), (2, "b")],
        ),
        ("SELECT * FROM unnest(ARRAY[5,6]) WITH ORDINALITY", [(5, 1), (6, 2)]),
        ("SELECT * FROM unnest(ARRAY['a']) AS t(v)", [("a",)]),
        ("SELECT * FROM generate_series(1,2) WITH ORDINALITY AS t(v, i)", [(1, 1), (2, 2)]),
    ],
)
def test_unnest_with_ordinality_column_aliases(store, sql, want):
    assert _rows(store, sql, Session(database="t")) == want


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # greatest/least take their arguments' type, as COALESCE does; typing
        # them text sent greatest(NULL, 1) as the STRING '1' under oid 25.
        ("SELECT greatest(NULL, 1), least(2, NULL)", [(1, 2)]),
        ("SELECT greatest(1, 2, 3), least(1, 2, 3)", [(3, 1)]),
        ("SELECT greatest('a','b')", [("b",)]),
        ("SELECT num_nonnulls(1, NULL, 2), num_nulls(1, NULL, 2)", [(2, 1)]),
        ("SELECT num_nonnulls(), num_nulls(NULL)", [(0, 1)]),
    ],
)
def test_greatest_least_and_null_counts(store, sql, want):
    assert _rows(store, sql, Session(database="t")) == want
