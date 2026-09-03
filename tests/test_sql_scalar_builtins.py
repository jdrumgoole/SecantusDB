"""Scalar builtins that were missing, and four wrong result TYPES.

Found 2026-09-01 by a broad expression sweep against PostgreSQL 14.13 — none
of it was on the backlog. Two distinct failures:

**Nine functions were simply absent.** They answered
`0A000 function <name>() is not supported in this context` — misleading
wording, because they were unreachable in EVERY context: FROM-less, over a
table, and over a column alike.

**Four expressions returned the wrong type**, which is the worse half because
it is silent. `coalesce(NULL, NULL, 3)` sent the STRING `'3'` with oid 25
where PostgreSQL sends `3` as int4, and `IS DISTINCT FROM` sent `'t'` as text
where PostgreSQL sends a boolean — the exact failure `_BOOL_EXPR_TYPES`
already existed to prevent, for two node types that were never added to it.

A separate cause sat under three of the nine: `_eval_typed_func` builds its
argument list from ``node.expressions`` only, so a sqlglot node that carries
its first argument in ``node.this`` (`MD5`, `StartsWith`, `WidthBucket`)
reached the implementation with an EMPTY arg list and answered NULL rather
than erroring — a wrong answer wearing the shape of a right one.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    run("CREATE TABLE sw (id int, s text)")
    run("INSERT INTO sw VALUES (1, ' ab ')")
    try:
        yield run
    finally:
        storage.close()


@pytest.mark.parametrize(
    ("expr", "value"),
    [
        ("md5('abc')", "900150983cd24fb0d6963f7d28e17f72"),
        ("btrim('  x  ')", "x"),
        ("btrim('xxaxx', 'x')", "a"),
        ("quote_ident('a b')", '"a b"'),
        ("quote_ident('abc')", "abc"),
        ("quote_literal('a''b')", "'a''b'"),
        ("quote_nullable(NULL)", "NULL"),
        ("concat_ws('-', 'a', NULL, 'b')", "a-b"),
    ],
)
def test_missing_string_builtins(db, expr, value):
    rows, _ = db(f"SELECT {expr}")
    assert rows == [(value,)]


@pytest.mark.parametrize(
    ("expr", "value", "tag"),
    [
        ("starts_with('abc', 'ab')", True, "bool"),
        ("starts_with('abc', 'zz')", False, "bool"),
        ("width_bucket(5.35, 0.024, 10.06, 5)", 3, "int4"),
        ("width_bucket(-1, 0, 10, 5)", 0, "int4"),
        ("width_bucket(99, 0, 10, 5)", 6, "int4"),
    ],
)
def test_builtins_with_a_non_text_return(db, expr, value, tag):
    rows, tags = db(f"SELECT {expr}")
    assert rows == [(value,)]
    assert tags == [tag]


def test_first_argument_in_node_this_is_not_dropped(db):
    """`MD5`, `StartsWith` and `WidthBucket` carry their first argument in
    ``node.this``, which the generic typed-function path drops — they answered
    NULL, not an error."""
    rows, _ = db("SELECT md5(s) FROM sw")
    assert rows != [(None,)]
    assert rows == [("65a4d88f1b7a39d950a5a3bb1f5c2c6c",)]


class TestResultTypes:
    """The silent half: right value, wrong type on the wire."""

    def test_coalesce_takes_its_arguments_type(self, db):
        rows, tags = db("SELECT coalesce(NULL, NULL, 3)")
        assert rows == [(3,)]
        assert tags == ["int4"]

    def test_coalesce_of_text_is_still_text(self, db):
        rows, tags = db("SELECT coalesce(NULL, 'x')")
        assert rows == [("x",)]
        assert tags == ["text"]

    @pytest.mark.parametrize(
        "expr",
        ["NULL::int IS DISTINCT FROM 1", "1 IS NOT DISTINCT FROM 1", "1 IS DISTINCT FROM 1"],
    )
    def test_is_distinct_from_is_boolean(self, db, expr):
        rows, tags = db(f"SELECT {expr}")
        assert tags == ["bool"]
        assert isinstance(rows[0][0], bool)

    @pytest.mark.parametrize("expr", ["power(2, 10)", "sign(-3)"])
    def test_power_and_sign_are_double_precision(self, db, expr):
        _rows, tags = db(f"SELECT {expr}")
        assert tags == ["float8"]

    def test_div_returns_numeric(self, db):
        rows, tags = db("SELECT div(7, 2)")
        assert rows == [(3,)]
        assert tags == ["numeric"]

    def test_div_truncates_toward_zero(self, db):
        assert db("SELECT div(-7, 2)")[0] == [(-3,)]

    def test_div_by_zero_is_22012(self, db):
        from secantus.sql.errors import SQLError

        with pytest.raises(SQLError) as ei:
            db("SELECT div(1, 0)")
        assert ei.value.sqlstate == "22012"


class TestAggregateExpressionArguments:
    """`bool_and(n > 0)` answered NULL — silently.

    `bool_and(b)` over a bare boolean COLUMN was always correct, which is what
    hid it. `_accumulator`'s expression-argument branch was gated on a function
    list that omitted the bool aggregates, so a comparison argument fell
    through to the field-path branch, arrived with `field=None`, and the
    accumulator body became literally `None`.

    Fixing the gate then exposed a second layer: `_agg_arg_to_expr` could not
    lower a comparison at all. It now lowers comparisons, `NOT`, and `CASE` —
    with SQL's three-valued logic, so a comparison against NULL stays NULL
    (Mongo's `$gt` would answer false, which would turn `bool_and` from true
    into false).
    """

    @pytest.fixture()
    def agg(self, tmp_path):
        storage = Storage(str(tmp_path))
        session = Session(database="t")

        def run(sql: str):
            return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

        run("CREATE TABLE ba (id int, n int, b bool)")
        run("INSERT INTO ba VALUES (1,5,true),(2,NULL,NULL),(3,3,true)")
        try:
            yield run
        finally:
            storage.close()

    @pytest.mark.parametrize(
        ("expr", "value"),
        [
            ("bool_and(b)", True),
            ("bool_or(b)", True),
            ("every(b)", True),
            ("bool_and(n > 0)", True),
            ("bool_or(n > 4)", True),
            ("bool_and(n > 9)", False),
            ("bool_or(n > 9)", False),
        ],
    )
    def test_bool_aggregates(self, agg, expr, value):
        assert agg(f"SELECT {expr} FROM ba") == [(value,)]

    def test_a_null_row_is_skipped_not_treated_as_false(self, agg):
        """Row 2 has `n IS NULL`, so `n > 0` is NULL there. PostgreSQL skips it
        and answers true; a plain `$gt` would answer false and flip the result."""
        assert agg("SELECT bool_and(n > 0) FROM ba") == [(True,)]

    def test_bool_and_over_no_rows_is_null(self, agg):
        assert agg("SELECT bool_and(b) FROM ba WHERE false") == [(None,)]

    def test_sum_of_a_case_expression(self, agg):
        assert agg("SELECT sum(CASE WHEN n > 3 THEN 1 ELSE 0 END) FROM ba") == [(1,)]

    def test_unsupported_argument_names_the_aggregate_not_array_agg(self, agg):
        """The message said "unsupported array_agg argument" for a `min()`
        call, sending the reader to the wrong function.

        The example was `min(abs(n))` until `abs` became lowerable
        (`_AGG_FUNC_OPS`); `md5` stands in for "a function with no Mongo
        operator", which is what this test needs. The assertion is about the
        MESSAGE, not about which functions happen to be unsupported.
        """
        from secantus.sql.errors import SQLError

        with pytest.raises(SQLError) as ei:
            agg("SELECT min(md5(b::text)) FROM ba")
        assert "array_agg" not in str(ei.value)
        assert "unsupported aggregate argument" in str(ei.value)
