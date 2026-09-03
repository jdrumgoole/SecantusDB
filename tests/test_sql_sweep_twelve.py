"""A twelfth differential sweep — LIMIT 0, and rows that contain NULL.

Transactions came back PERFECT (40 of 40): savepoints, the aborted-transaction
state, isolation levels, `READ ONLY`, and transactional DDL rollback all match
PostgreSQL 14.13. Two clusters did not.

**`LIMIT 0` returned every row.** A sentinel collision: the planner used `0` to
mean "no LIMIT", and every consumer tested the value for truthiness, so a real
`LIMIT 0` looked like its own absence. It matters because `LIMIT 0` is how a
client asks for a result's column metadata WITHOUT any rows — ORMs and BI tools
do it constantly — and the answer was the whole table.

The fix runs deeper than the sentinel: the storage layer reads `limit=0` as "no
limit" too (Mongo's convention), so a genuine `LIMIT 0` must never reach it, and
Mongo's `$limit` stage REJECTS zero outright (`54000 the limit must be
positive`), so the pipeline emits a match-nothing stage instead.

**Rows containing NULL ignored SQL's three-valued rules.** `(1,NULL) =
(1,NULL)` answered true where PostgreSQL says NULL, `(NULL,NULL) IS NULL`
answered false where it says true, and `(1,2) < (1,NULL)` raised `42883` naming
`integer[]` — the record was being compared as an array.

Note `row IS NOT NULL` is **not** the negation of `row IS NULL`: the first is
true only when every field is non-NULL, the second only when every field is
NULL, so a row with one NULL is false for both.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "s12"))
    try:
        yield s
    finally:
        s.close()


def _rows(store, sql, session):
    return [r for r in run_sql(store, "t", sql, session=session)][0].rows


@pytest.fixture
def sess(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE mi12 (id int PRIMARY KEY, s text)", s)
    _rows(store, "INSERT INTO mi12 VALUES (1,'a'), (2,NULL)", s)
    return s


# --------------------------------------------------------------------------- #
# LIMIT 0 is no rows, not "no limit"
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT id FROM mi12 ORDER BY id LIMIT 0",
        "SELECT id FROM mi12 LIMIT 0",
        "SELECT id FROM mi12 ORDER BY id LIMIT 0 OFFSET 0",
        # A grouped query takes the pipeline path, where Mongo's `$limit` stage
        # rejects zero outright.
        "SELECT s FROM mi12 GROUP BY s LIMIT 0",
        # ...and a FROM-less SELECT has no storage access at all.
        "SELECT 1 LIMIT 0",
        "SELECT version() LIMIT 0",
    ],
)
def test_limit_zero_returns_no_rows(store, sess, sql):
    assert _rows(store, sql, sess) == []


def test_limit_zero_in_a_derived_table(store, sess):
    assert _rows(store, "SELECT count(*) FROM (SELECT id FROM mi12 LIMIT 0) q", sess) == [(0,)]


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # `LIMIT NULL` and `LIMIT ALL` are Postgres' spellings of "no limit".
        ("SELECT id FROM mi12 ORDER BY id LIMIT NULL", [(1,), (2,)]),
        ("SELECT id FROM mi12 ORDER BY id LIMIT ALL", [(1,), (2,)]),
        ("SELECT id FROM mi12 ORDER BY id LIMIT 1", [(1,)]),
        ("SELECT id FROM mi12 ORDER BY id LIMIT 99", [(1,), (2,)]),
        ("SELECT id FROM mi12 ORDER BY id OFFSET 1", [(2,)]),
        # `FETCH FIRST ROW ONLY` — the count is optional and defaults to one.
        ("SELECT id FROM mi12 ORDER BY id FETCH FIRST ROW ONLY", [(1,)]),
        ("SELECT id FROM mi12 ORDER BY id FETCH FIRST 1 ROW ONLY", [(1,)]),
        ("SELECT id FROM mi12 ORDER BY id OFFSET 1 ROW FETCH NEXT 1 ROW ONLY", [(2,)]),
        ("SELECT 1 OFFSET 1", []),
    ],
)
def test_limit_and_offset_spellings(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


def test_negative_limit_is_rejected(store, sess):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT id FROM mi12 LIMIT -1", sess)
    assert exc.value.sqlstate == "2201W"


# --------------------------------------------------------------------------- #
# Rows containing NULL
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # Nothing decided it, so the answer is unknown — not "the tuples match".
        ("SELECT (1,NULL) = (1,NULL)", None),
        ("SELECT ROW(1,NULL) = ROW(1,NULL)", None),
        ("SELECT (1,NULL) = (1,2)", None),
        # ...but an earlier field CAN decide it.
        ("SELECT (1,NULL) = (2,3)", False),
        ("SELECT (1,2) = (1,2)", True),
        ("SELECT (1,2) < (1,3)", True),
        # An ordering that reaches a NULL pair is unknown; this used to raise.
        ("SELECT (1,2) < (1,NULL)", None),
    ],
)
def test_row_comparison_with_nulls(store, sess, sql, want):
    assert _rows(store, sql, sess) == [(want,)]


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # `IS NULL` needs EVERY field NULL...
        ("SELECT (NULL,NULL) IS NULL", True),
        ("SELECT (1,NULL) IS NULL", False),
        ("SELECT (1,2) IS NULL", False),
        # ...and `IS NOT NULL` needs every field NON-null, so a row with one
        # NULL is false for BOTH. It is not the negation.
        ("SELECT (NULL,NULL) IS NOT NULL", False),
        ("SELECT (1,NULL) IS NOT NULL", False),
        ("SELECT (1,2) IS NOT NULL", True),
    ],
)
def test_row_is_null(store, sess, sql, want):
    assert _rows(store, sql, sess) == [(want,)]


def test_scalar_is_null_is_unchanged(store, sess):
    assert _rows(store, "SELECT NULL IS NULL, NULL IS NOT NULL, 1 IS NOT NULL", sess) == [
        (True, False, True)
    ]
