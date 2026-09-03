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


# --------------------------------------------------------------------------- #
# An aggregate's argument may be a function call
# --------------------------------------------------------------------------- #


@pytest.fixture
def ag(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE ag13 (id int PRIMARY KEY, g text, s text, n int)", s)
    _rows(store, "INSERT INTO ag13 VALUES (1,'a','x',10),(2,'a',NULL,20),(3,'b','y',5)", s)
    return s


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # All of these answered `0A000 unsupported aggregate argument`: the
        # lowerer handled columns, literals, comparisons, CASE and arithmetic,
        # but no function calls at all.
        ("SELECT sum(coalesce(n,0)) FROM ag13", 35),
        ("SELECT sum(abs(n)) FROM ag13", 35),
        ("SELECT max(coalesce(s,'-')) FROM ag13", "y"),
        ("SELECT min(lower(g)) FROM ag13", "a"),
        ("SELECT count(coalesce(s,'-')) FROM ag13", 3),
        ("SELECT string_agg(coalesce(s,'-'), ',' ORDER BY id) FROM ag13", "x,-,y"),
        ("SELECT string_agg(s || '!', ',' ORDER BY id) FROM ag13", "x!,y!"),
    ],
)
def test_aggregate_over_a_function_call(store, ag, sql, want):
    assert _rows(store, sql, ag)[0][0] == want


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # Postgres' scalar functions are STRICT — a NULL in is a NULL out.
        # Mongo's are not: `$toUpper` maps null to '' and `$strLenCP` rejects it
        # outright (which escaped as XX000), so each needs a guard.
        ("SELECT array_agg(upper(s) ORDER BY id) FROM ag13", ["X", None, "Y"]),
        ("SELECT array_agg(length(s) ORDER BY id) FROM ag13", [1, None, 1]),
        ("SELECT array_agg(coalesce(s,'-') ORDER BY id) FROM ag13", ["x", "-", "y"]),
    ],
)
def test_aggregate_function_argument_is_strict_on_null(store, ag, sql, want):
    assert _rows(store, sql, ag)[0][0] == want


def test_round_is_deliberately_not_lowered(store, ag):
    # Mongo's `$round` rounds half-to-EVEN where Postgres rounds half away from
    # zero, so lowering it would answer 4 for `sum(round(x))` over 1.5 and 2.5
    # where PG says 5 — a silent wrong answer in place of an honest error.
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT sum(round(n / 4.0)) FROM ag13", ag)
    assert exc.value.sqlstate == "0A000"


def test_grouped_aggregate_over_a_function_call(store, ag):
    rows = _rows(
        store,
        "SELECT g, string_agg(coalesce(s,'-'), ',' ORDER BY id) FROM ag13 GROUP BY g ORDER BY g",
        ag,
    )
    assert rows == [("a", "x,-"), ("b", "y")]


# --------------------------------------------------------------------------- #
# NATURAL JOIN was a cross join
# --------------------------------------------------------------------------- #


@pytest.fixture
def nat(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE j14 (id int PRIMARY KEY, g text, n int)", s)
    _rows(store, "CREATE TABLE k14 (id int PRIMARY KEY, jid int, v int)", s)
    _rows(store, "INSERT INTO j14 VALUES (1,'a',10),(2,'b',20),(3,'c',NULL)", s)
    _rows(store, "INSERT INTO k14 VALUES (10,1,100),(11,1,200),(12,2,300)", s)
    return s


def test_natural_join_uses_the_common_column(store, nat):
    # `id` is the only common column and no j14.id equals a k14.id, so the
    # answer is empty. It used to be all 9 pairs — the condition was dropped.
    assert _rows(store, "SELECT count(*) FROM j14 NATURAL JOIN k14", nat) == [(0,)]


def test_natural_left_join(store, nat):
    # This did not merely return the wrong rows — it errored outright with
    # "LEFT JOIN requires an ON clause".
    assert _rows(store, "SELECT count(*) FROM j14 NATURAL LEFT JOIN k14", nat) == [(3,)]


def test_natural_join_that_does_match(store, nat):
    rows = _rows(
        store,
        "SELECT j.id, k.v FROM j14 j NATURAL JOIN (SELECT jid AS id, v FROM k14) k ORDER BY k.v",
        nat,
    )
    assert rows == [(1, 100), (1, 200), (2, 300)]


def test_using_join_still_works(store, nat):
    # NATURAL desugars to USING, so the existing path must stay intact.
    assert _rows(store, "SELECT count(*) FROM j14 j JOIN k14 k USING (id)", nat) == [(0,)]


def test_cross_join_is_unaffected(store, nat):
    assert _rows(store, "SELECT count(*) FROM j14 CROSS JOIN k14", nat) == [(9,)]


def test_scalar_is_null_is_unchanged(store, sess):
    assert _rows(store, "SELECT NULL IS NULL, NULL IS NOT NULL, 1 IS NOT NULL", sess) == [
        (True, False, True)
    ]
