"""An eighth differential sweep — json navigation, quantifiers, ORDER BY.

263 statements run against PostgreSQL 14.13 through the SAME psycopg client on
both sides, so client-side type mapping is identical and every difference is
the server's. It found four SILENTLY WRONG answers, which is what makes this
sweep worth its length:

**Every navigation over a ``json`` (non-``b``) value answered NULL.** The
``json`` type keeps the client's exact text -- that is what separates it from
``jsonb`` -- so a ``::json`` value is a ``JsonText``, a *str subclass*. The
``->`` / ``->>`` / ``#>`` / ``#>>`` walker descends ``dict`` and ``list`` only,
so it fell through to "not a container" and answered NULL for the entire type.

**``ORDER BY b.id`` sorted by ``a.id``.** Two joined tables routinely project
same-named columns; the order term was resolved by its BARE name against the
output list, which finds the first of them. In a RIGHT JOIN it also mis-placed
the unmatched rows, because their ``a.id`` is NULL.

**``SELECT jsonb_each(x)`` returned no rows.** The SELECT-list record-SRF
expansion was written for ``_pg_expandarray`` and treats the argument as an
array; ``jsonb_each``'s argument is an object, so it expanded to zero elements.

**``ORDER BY 99`` was accepted and ignored.** Each planning path gates on
``1 <= n <= len(select list)`` and, when that fails, leaves the literal alone --
which sorts by a constant. Postgres raises 42P10.

Plus: ``#>`` typed as text (its comment claimed jsonb -- ``#>`` parses to
``JSONBExtract``, which is NOT a subclass of the ``JSONExtract`` the inference
named); ``LIKE ... ESCAPE`` typed as text where PG sends a boolean; ``SIMILAR
TO`` was unimplemented; ``array_agg(DISTINCT x)`` was a hard error; quantified
comparisons worked only under ``=``/``<>``; ``ON a.id = b.id - 1`` was refused
outright; and an incomparable pair leaked ``XX000 internal error`` with a Python
traceback behind it.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "s8"))
    try:
        yield s
    finally:
        s.close()


def _rows(store, sql, session):
    return [r for r in run_sql(store, "t", sql, session=session)][0].rows


def _tags(store, sql, session):
    res = [r for r in run_sql(store, "t", sql, session=session)][0]
    return [c.type_tag for c in res.columns]


@pytest.fixture
def sess(store):
    s = Session(database="t")
    _rows(store, "CREATE TABLE s8 (id int PRIMARY KEY, n int, s text)", s)
    _rows(store, "INSERT INTO s8 VALUES (1,10,'alpha'),(2,-5,'Beta'),(3,0,'g'),(4,7,NULL)", s)
    return s


# --------------------------------------------------------------------------- #
# json (non-b) navigation — every operator answered NULL
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("""SELECT '{"a":1}'::json -> 'a'""", [(1,)]),
        ("""SELECT '{"a":1}'::json ->> 'a'""", [("1",)]),
        ("""SELECT '{"a":{"b":2}}'::json -> 'a' -> 'b'""", [(2,)]),
        ("""SELECT '[1,2]'::json -> 0""", [(1,)]),
        ("""SELECT '{"a":1}'::json #> '{a}'""", [(1,)]),
        ("""SELECT '{"a":1}'::json #>> '{a}'""", [("1",)]),
        ("""SELECT json_extract_path('{"a":{"b":2}}'::json, 'a', 'b')""", [(2,)]),
    ],
)
def test_json_navigation_descends_into_json_text(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


# --------------------------------------------------------------------------- #
# `#>` keeps jsonb; `#>>` returns text
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "tag"),
    [
        ("""SELECT '{"a":{"b":2}}'::jsonb #> '{a,b}'""", "json"),
        ("""SELECT '{"a":{"b":2}}'::jsonb #>> '{a,b}'""", "text"),
        ("""SELECT '{"a":1}'::jsonb -> 'a'""", "json"),
        ("""SELECT '{"a":1}'::jsonb ->> 'a'""", "text"),
    ],
)
def test_jsonb_path_navigation_types(store, sess, sql, tag):
    assert _tags(store, sql, sess) == [tag]


def test_hash_gt_returns_a_container_not_its_array_rendering(store, sess):
    # Typed text, the nested array came back as the Postgres ARRAY literal
    # `{1,2}` rather than the jsonb `[1, 2]`.
    assert _rows(store, """SELECT '{"a":{"b":[1,2]}}'::jsonb #> '{a,b}'""", sess) == [([1, 2],)]


# --------------------------------------------------------------------------- #
# jsonb function names that only existed in their `json_` spelling
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("""SELECT jsonb_extract_path('{"a":{"b":2}}'::jsonb, 'a', 'b')""", [(2,)]),
        ("""SELECT jsonb_extract_path_text('{"a":{"b":2}}'::jsonb, 'a', 'b')""", [("2",)]),
        ("""SELECT jsonb_path_query_first('{"a":[1,2]}'::jsonb, '$.a[*]')""", [(1,)]),
        ("SELECT array_to_json(ARRAY[1,2])", [([1, 2],)]),
    ],
)
def test_jsonb_functions_present(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


# --------------------------------------------------------------------------- #
# `SELECT jsonb_each(x)` returned NO ROWS
# --------------------------------------------------------------------------- #


def test_select_list_jsonb_each_expands(store, sess):
    rows = _rows(store, """SELECT (jsonb_each('{"a":1,"b":2}'::jsonb)).key""", sess)
    assert rows == [("a",), ("b",)]


def test_select_list_jsonb_each_value_field(store, sess):
    rows = _rows(store, """SELECT (jsonb_each('{"a":1,"b":2}'::jsonb)).value""", sess)
    assert rows == [(1,), (2,)]


def test_select_list_jsonb_each_composite_row_count(store, sess):
    # The composite form: one row per member, where there used to be none.
    assert len(_rows(store, """SELECT jsonb_each('{"a":1,"b":2}'::jsonb)""", sess)) == 2


# --------------------------------------------------------------------------- #
# ORDER BY position out of range
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize("sql", ["SELECT id FROM s8 ORDER BY 99", "SELECT id FROM s8 ORDER BY 0"])
def test_order_by_position_out_of_range(store, sess, sql):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, sql, sess)
    assert exc.value.sqlstate == "42P10"


def test_order_by_negative_position(store, sess):
    # `-1` parses as Neg(Literal), which never reached the range gate at all.
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT id FROM s8 ORDER BY -1", sess)
    assert exc.value.sqlstate == "42P10"


def test_order_by_valid_position_still_works(store, sess):
    assert _rows(store, "SELECT id FROM s8 ORDER BY 1 DESC", sess) == [(4,), (3,), (2,), (1,)]


# --------------------------------------------------------------------------- #
# A qualified ORDER BY term must not collide with a same-named output column
# --------------------------------------------------------------------------- #


def test_order_by_qualified_column_picks_the_right_side(store, sess):
    rows = _rows(
        store,
        "SELECT a.id, b.id FROM s8 a JOIN s8 b ON a.id = 5 - b.id ORDER BY b.id",
        sess,
    )
    assert rows == [(4, 1), (3, 2), (2, 3), (1, 4)]


def test_right_join_orders_unmatched_rows_by_the_named_column(store, sess):
    # The unmatched row's `a.id` is NULL, so sorting by the WRONG `id` pushed it
    # to the end (ASC) or the front (DESC) instead of into its place.
    rows = _rows(
        store,
        "SELECT a.id, b.id FROM s8 a RIGHT JOIN s8 b ON a.id = b.id - 1 ORDER BY b.id",
        sess,
    )
    assert rows == [(None, 1), (1, 2), (2, 3), (3, 4)]


# --------------------------------------------------------------------------- #
# JOIN ON with an expression operand
# --------------------------------------------------------------------------- #


def test_join_on_expression(store, sess):
    rows = _rows(
        store, "SELECT a.id, b.id FROM s8 a JOIN s8 b ON a.id = b.id - 1 ORDER BY a.id", sess
    )
    assert rows == [(1, 2), (2, 3), (3, 4)]


def test_left_join_on_expression_keeps_unmatched(store, sess):
    rows = _rows(
        store, "SELECT a.id, b.id FROM s8 a LEFT JOIN s8 b ON a.id = b.id - 1 ORDER BY a.id", sess
    )
    assert rows == [(1, 2), (2, 3), (3, 4), (4, None)]


# --------------------------------------------------------------------------- #
# SIMILAR TO
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # `.` is LITERAL in a SIMILAR TO pattern — the whole trap of the dialect.
        ("SELECT 'abc' SIMILAR TO 'a.c'", [(False,)]),
        ("SELECT 'a.c' SIMILAR TO 'a.c'", [(True,)]),
        ("SELECT 'abc' SIMILAR TO 'a%'", [(True,)]),
        ("SELECT 'abc' SIMILAR TO 'a_c'", [(True,)]),
        ("SELECT 'abc' SIMILAR TO '(a|b)bc'", [(True,)]),
        ("SELECT 'abc' SIMILAR TO '[a-c]bc'", [(True,)]),
        ("SELECT 'xbc' SIMILAR TO '[a-c]bc'", [(False,)]),
        ("SELECT 'aabc' SIMILAR TO 'a{1,2}bc'", [(True,)]),
        # The match is against the WHOLE string.
        ("SELECT 'abc' SIMILAR TO 'ab'", [(False,)]),
        ("SELECT 'abc' NOT SIMILAR TO 'a%'", [(False,)]),
        ("SELECT NULL SIMILAR TO 'a'", [(None,)]),
        ("SELECT 'a' SIMILAR TO NULL", [(None,)]),
        ("SELECT 'a%b' SIMILAR TO 'a#%b' ESCAPE '#'", [(True,)]),
    ],
)
def test_similar_to(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT 'abc' SIMILAR TO 'a%'",
        "SELECT 'a%b' SIMILAR TO 'a#%b' ESCAPE '#'",
        "SELECT 'a%b' LIKE 'a#%b' ESCAPE '#'",
    ],
)
def test_pattern_predicates_type_as_boolean(store, sess, sql):
    # An ESCAPE clause wraps the predicate in a node that is not itself a
    # boolean class, so adding it flipped a working LIKE from bool to text.
    assert _tags(store, sql, sess) == ["bool"]


# --------------------------------------------------------------------------- #
# Quantified comparisons: every operator, and SQL three-valued logic
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("SELECT 1 < ALL(ARRAY[2,3])", [(True,)]),
        ("SELECT 1 < ANY(ARRAY[2,3])", [(True,)]),
        ("SELECT 5 > ALL(ARRAY[2,3])", [(True,)]),
        ("SELECT 5 >= ALL(ARRAY[5,3])", [(True,)]),
        ("SELECT 1 <= ALL(ARRAY[1,2])", [(True,)]),
        ("SELECT 3 <> ALL(ARRAY[1,2])", [(True,)]),
        ("SELECT 'b' < ALL(ARRAY['c','d'])", [(True,)]),
        # An EMPTY array settles it before the needle is looked at.
        ("SELECT 1 < ALL(ARRAY[]::int[])", [(True,)]),
        ("SELECT 1 < ANY(ARRAY[]::int[])", [(False,)]),
        ("SELECT NULL = ALL(ARRAY[]::int[])", [(True,)]),
        ("SELECT NULL = ANY(ARRAY[]::int[])", [(False,)]),
        # …but a NULL array, or a NULL that leaves the answer open, is NULL.
        ("SELECT NULL = ALL(ARRAY[1])", [(None,)]),
        ("SELECT 1 = ALL(ARRAY[1,NULL])", [(None,)]),
        ("SELECT 1 = ALL(ARRAY[2,NULL])", [(False,)]),
        ("SELECT 1 = ANY(ARRAY[1,NULL])", [(True,)]),
        ("SELECT 1 = ANY(ARRAY[2,NULL])", [(None,)]),
        ("SELECT 1 = ALL(NULL::int[])", [(None,)]),
        ("SELECT 1 = ANY(NULL::int[])", [(None,)]),
    ],
)
def test_quantified_comparisons(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


def test_quantified_subquery_any(store, sess):
    rows = _rows(
        store, "SELECT id FROM s8 WHERE id = ANY(SELECT id FROM s8 WHERE id < 3) ORDER BY id", sess
    )
    assert rows == [(1,), (2,)]


def test_quantified_subquery_all(store, sess):
    rows = _rows(
        store, "SELECT id FROM s8 WHERE id > ALL(SELECT id FROM s8 WHERE id < 3) ORDER BY id", sess
    )
    assert rows == [(3,), (4,)]


# --------------------------------------------------------------------------- #
# Aggregate DISTINCT — Postgres dedupes by SORTING, so the result is ordered
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("SELECT array_agg(DISTINCT n) FROM s8", [([-5, 0, 7, 10],)]),
        ("SELECT array_agg(DISTINCT n ORDER BY n) FROM s8", [([-5, 0, 7, 10],)]),
        ("SELECT array_agg(DISTINCT n ORDER BY n DESC) FROM s8", [([10, 7, 0, -5],)]),
        ("SELECT string_agg(DISTINCT s, ',') FROM s8", [("Beta,alpha,g",)]),
        ("SELECT string_agg(DISTINCT s, ',' ORDER BY s) FROM s8", [("Beta,alpha,g",)]),
    ],
)
def test_aggregate_distinct(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


def test_distinct_aggregate_order_by_must_be_the_argument(store, sess):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT array_agg(DISTINCT n ORDER BY id) FROM s8", sess)
    assert exc.value.sqlstate == "42P10"


# --------------------------------------------------------------------------- #
# String functions
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        # A DIGIT does not start a new word — `str.title()` thinks it does.
        ("SELECT initcap('a1b c')", [("A1b C",)]),
        ("SELECT initcap('ab1cd')", [("Ab1cd",)]),
        ("SELECT initcap('1abc')", [("1abc",)]),
        ("SELECT initcap('x9y z9')", [("X9y Z9",)]),
        ("SELECT initcap('a-b')", [("A-B",)]),
        ("SELECT initcap('o''brien')", [("O'Brien",)]),
        # quote_ident quotes a keyword that is not UNRESERVED.
        ("SELECT quote_ident('select')", [('"select"',)]),
        ("SELECT quote_ident('abc')", [("abc",)]),
        ("SELECT quote_ident('abort')", [("abort",)]),
        # %I *is* quote_ident, so it quotes only when it must.
        ("SELECT format('%I', 'tbl')", [("tbl",)]),
        ("SELECT format('%I', 'a1')", [("a1",)]),
        ("SELECT format('%I', 'My Tbl')", [('"My Tbl"',)]),
        ("SELECT format('%I', 'select')", [('"select"',)]),
    ],
)
def test_string_functions(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


def test_format_identifier_rejects_null(store, sess):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT format('%I', NULL)", sess)
    assert exc.value.sqlstate == "22004"


# --------------------------------------------------------------------------- #
# extract / date_trunc fields that were absent
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("SELECT extract(isoyear FROM DATE '2021-01-03')", 2020),
        ("SELECT extract(julian FROM DATE '2020-01-01')", 2458850),
        # Postgres folds the SECONDS into these.
        ("SELECT extract(microseconds FROM TIMESTAMP '2020-01-15 10:30:45.5')", 45500000),
        ("SELECT extract(milliseconds FROM TIMESTAMP '2020-01-15 10:30:45.5')", 45500),
    ],
)
def test_extract_fields(store, sess, sql, want):
    assert _rows(store, sql, sess)[0][0] == want


@pytest.mark.parametrize(
    ("unit", "year"),
    [("decade", 2020), ("century", 2001), ("millennium", 2001)],
)
def test_date_trunc_wide_units(store, sess, unit, year):
    # Centuries and millennia START at year 1, so 2026 truncates to 2001.
    rows = _rows(store, f"SELECT date_trunc('{unit}', TIMESTAMP '2026-05-17 10:30:00')", sess)
    assert (rows[0][0].year, rows[0][0].month, rows[0][0].day) == (year, 1, 1)


def test_date_trunc_milliseconds(store, sess):
    rows = _rows(
        store, "SELECT date_trunc('milliseconds', TIMESTAMP '2020-05-17 10:30:00.123456')", sess
    )
    assert rows[0][0].microsecond == 123000


# --------------------------------------------------------------------------- #
# generate_series over date / timestamp bounds
# --------------------------------------------------------------------------- #


def test_generate_series_date_bounds(store, sess):
    rows = _rows(
        store,
        "SELECT generate_series(DATE '2020-01-01', DATE '2020-01-03', INTERVAL '1 day')",
        sess,
    )
    assert [r[0].day for r in rows] == [1, 2, 3]


def test_generate_series_date_bounds_type(store, sess):
    # Postgres resolves DATE bounds to the timestamptz overload, TIMESTAMP
    # bounds to the plain timestamp one.
    assert _tags(
        store,
        "SELECT generate_series(DATE '2020-01-01', DATE '2020-01-02', INTERVAL '1 day')",
        sess,
    ) == ["timestamptz"]
    assert _tags(
        store,
        "SELECT generate_series(TIMESTAMP '2020-01-01', TIMESTAMP '2020-01-02', INTERVAL '1 day')",
        sess,
    ) == ["timestamp"]


# --------------------------------------------------------------------------- #
# Error surface
# --------------------------------------------------------------------------- #


def test_unknown_function_is_42883(store, sess):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, "SELECT nonexistent_fn(1)", sess)
    assert exc.value.sqlstate == "42883"
    assert "does not exist" in str(exc.value)


def test_set_returning_function_in_scalar_position_is_0a000(store, sess):
    # The NAME is real; the position is what this engine cannot serve, so
    # "does not exist" would be a lie.
    with pytest.raises(errors.SQLError) as exc:
        _rows(
            store,
            """SELECT jsonb_array_length('[1,2]'::jsonb), """
            """jsonb_object_keys('{"a":1}'::jsonb)""",
            sess,
        )
    assert exc.value.sqlstate == "0A000"


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT ARRAY[1,2] > 1",
        """SELECT '{"a":1}'::jsonb > 1""",
    ],
)
def test_incomparable_operands_raise_42883_not_an_internal_error(store, sess, sql):
    # These raised a bare Python TypeError, which reached the client as XX000.
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, sql, sess)
    assert exc.value.sqlstate == "42883"


# --------------------------------------------------------------------------- #
# trim_array
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("SELECT trim_array(ARRAY[1,2,3], 1)", [([1, 2],)]),
        ("SELECT trim_array(ARRAY[1,2,3], 0)", [([1, 2, 3],)]),
        ("SELECT trim_array(ARRAY[1,2,3], 3)", [([],)]),
    ],
)
def test_trim_array(store, sess, sql, want):
    assert _rows(store, sql, sess) == want


@pytest.mark.parametrize("n", [4, -1])
def test_trim_array_out_of_range(store, sess, n):
    with pytest.raises(errors.SQLError) as exc:
        _rows(store, f"SELECT trim_array(ARRAY[1,2,3], {n})", sess)
    assert exc.value.sqlstate == "2202E"
    assert "between 0 and 3" in str(exc.value)
