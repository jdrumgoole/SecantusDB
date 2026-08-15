"""generate_series + base-less FROM-clause set-returning functions (#125):
``generate_series`` (SELECT-list and FROM forms), ``FROM unnest(...)`` /
``jsonb_array_elements`` / ``regexp_split_to_table``, ``WITH ORDINALITY``, and
column/table aliases — all with projection / WHERE / ORDER BY / LIMIT.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


def _run(sql, sess=None, st=None):
    sess = sess or Session(database=DB)
    if st is not None:
        return run_sql(st, DB, sql, session=sess)[-1]
    st = Storage(":memory:")
    try:
        return run_sql(st, DB, sql, session=sess)[-1]
    finally:
        st.close()


def _rows(sql):
    return _run(sql).rows


def _cols(sql):
    r = _run(sql)
    return [c.name for c in r.columns]


# --------------------------------------------------------------------------- #
# generate_series
# --------------------------------------------------------------------------- #


def test_generate_series_fromless():
    assert _rows("SELECT generate_series(1, 5)") == [(1,), (2,), (3,), (4,), (5,)]
    assert _cols("SELECT generate_series(1, 5)") == ["generate_series"]


def test_generate_series_from():
    assert _rows("SELECT * FROM generate_series(1, 4)") == [(1,), (2,), (3,), (4,)]


def test_generate_series_step():
    assert _rows("SELECT * FROM generate_series(1, 10, 2)") == [(1,), (3,), (5,), (7,), (9,)]


def test_generate_series_negative_step():
    assert _rows("SELECT * FROM generate_series(5, 1, -2)") == [(5,), (3,), (1,)]


def test_generate_series_empty_and_single():
    assert _rows("SELECT * FROM generate_series(5, 1)") == []
    assert _rows("SELECT * FROM generate_series(3, 3)") == [(3,)]


def test_generate_series_zero_step_errors():
    with pytest.raises(errors.SQLError) as exc:
        _rows("SELECT * FROM generate_series(1, 5, 0)")
    assert exc.value.sqlstate == "22023"


def test_generate_series_count():
    assert _rows("SELECT count(*) FROM generate_series(1, 100)") == [(100,)]


# --------------------------------------------------------------------------- #
# Aliases, WHERE / ORDER BY / LIMIT
# --------------------------------------------------------------------------- #


def test_table_alias_names_column():
    # FROM generate_series(1,5) AS g -> the single column is named g.
    assert _cols("SELECT * FROM generate_series(1, 3) AS g") == ["g"]
    assert _rows("SELECT g FROM generate_series(1, 3) AS g") == [(1,), (2,), (3,)]


def test_column_alias():
    assert _cols("SELECT * FROM generate_series(1, 3) AS g(n)") == ["n"]
    assert _rows("SELECT n FROM generate_series(1, 3) AS g(n) WHERE n > 1") == [(2,), (3,)]


def test_order_by_and_limit():
    assert _rows("SELECT * FROM generate_series(1, 5) AS g ORDER BY g DESC LIMIT 2") == [(5,), (4,)]


# --------------------------------------------------------------------------- #
# WITH ORDINALITY
# --------------------------------------------------------------------------- #


def test_with_ordinality():
    r = _run("SELECT * FROM generate_series(10, 30, 10) WITH ORDINALITY")
    assert [c.name for c in r.columns] == ["generate_series", "ordinality"]
    assert r.rows == [(10, 1), (20, 2), (30, 3)]


def test_with_ordinality_aliased():
    r = _run("SELECT * FROM generate_series(1, 3) WITH ORDINALITY AS t(v, ord)")
    assert [c.name for c in r.columns] == ["v", "ord"]
    assert r.rows == [(1, 1), (2, 2), (3, 3)]


# --------------------------------------------------------------------------- #
# Other FROM-clause SRFs
# --------------------------------------------------------------------------- #


def test_from_unnest():
    assert _rows("SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x") == [(10,), (20,), (30,)]
    assert _cols("SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x") == ["x"]


def test_from_regexp_split_to_table():
    assert _rows("SELECT * FROM regexp_split_to_table('a,b,c', ',') AS p") == [
        ("a",),
        ("b",),
        ("c",),
    ]


def test_from_jsonb_array_elements():
    assert _rows("SELECT * FROM jsonb_array_elements('[1,2,3]'::jsonb) AS e") == [(1,), (2,), (3,)]


def test_from_jsonb_object_keys():
    got = _rows('SELECT * FROM jsonb_object_keys(\'{"a":1,"b":2}\'::jsonb) AS k')
    assert sorted(r[0] for r in got) == ["a", "b"]


# --------------------------------------------------------------------------- #
# Errors
# --------------------------------------------------------------------------- #


def test_generate_series_timestamp_step_zero_rejected():
    sql = (
        "SELECT * FROM generate_series(timestamp '2026-01-01', "
        "timestamp '2026-01-03', interval '0 day')"
    )
    with pytest.raises(errors.SQLError) as exc:
        _rows(sql)
    assert exc.value.sqlstate == "22023"


# --------------------------------------------------------------------------- #
# generate_series over timestamps + interval step (#150)
# --------------------------------------------------------------------------- #

import datetime as _dt  # noqa: E402


def test_generate_series_timestamp_day_step():
    rows = _rows(
        "SELECT * FROM generate_series(timestamp '2024-01-01', "
        "timestamp '2024-01-03', interval '1 day')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 1),),
        (_dt.datetime(2024, 1, 2),),
        (_dt.datetime(2024, 1, 3),),
    ]


def test_generate_series_timestamp_sub_day_step():
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-01 00:00', "
        "timestamp '2024-01-02 00:00', interval '12 hours')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 1, 0, 0),),
        (_dt.datetime(2024, 1, 1, 12, 0),),
        (_dt.datetime(2024, 1, 2, 0, 0),),
    ]


def test_generate_series_timestamp_month_step_clamps():
    # month stepping is calendar-aware; day clamps to month length (Jan 31 -> Feb 29)
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-31', timestamp '2024-03-31', interval '1 month')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 31),),
        (_dt.datetime(2024, 2, 29),),
        (_dt.datetime(2024, 3, 29),),
    ]


def test_generate_series_timestamp_descending():
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-03', timestamp '2024-01-01', interval '-1 day')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 3),),
        (_dt.datetime(2024, 1, 2),),
        (_dt.datetime(2024, 1, 1),),
    ]


def test_generate_series_timestamp_empty_when_wrong_direction():
    # positive step but start > stop -> no rows (Postgres)
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-03', timestamp '2024-01-01', interval '1 day')"
    )
    assert rows == []


def test_generate_series_timestamp_column_type():
    r = _run(
        "SELECT * FROM generate_series(timestamp '2024-01-01', "
        "timestamp '2024-01-02', interval '1 day')"
    )
    assert [c.type_tag for c in r.columns] == ["timestamp"]


# --------------------------------------------------------------------------- #
# regexp_matches as a set-returning function (#152)
# --------------------------------------------------------------------------- #


def test_regexp_matches_global_one_row_per_match():
    assert _rows("SELECT * FROM regexp_matches('foobarbaz', 'ba.', 'g') AS m") == [
        (["bar"],),
        (["baz"],),
    ]


def test_regexp_matches_capture_groups():
    assert _rows("SELECT * FROM regexp_matches('a1b2', '([a-z])([0-9])', 'g') AS m") == [
        (["a", "1"],),
        (["b", "2"],),
    ]


def test_regexp_matches_no_global_first_match_only():
    assert _rows("SELECT regexp_matches('a1b2c3', '([a-z])([0-9])')") == [(["a", "1"],)]


def test_regexp_matches_select_list_form():
    assert _rows("SELECT regexp_matches('xxaxxbxx', 'x(a|b)x', 'g')") == [(["a"],), (["b"],)]


def test_regexp_matches_no_match_no_rows():
    assert _rows("SELECT * FROM regexp_matches('abc', 'z', 'g') AS m") == []


def test_regexp_matches_case_insensitive_flag():
    assert _rows("SELECT * FROM regexp_matches('AbaBA', 'a', 'gi') AS m") == [
        (["A"],),
        (["a"],),
        (["A"],),
    ]


def test_regexp_matches_column_type_is_text_array():
    r = _run("SELECT * FROM regexp_matches('ab', '(a)(b)') AS m")
    assert [c.type_tag for c in r.columns] == ["text[]"]


# --------------------------------------------------------------------------- #
# jsonb_each / jsonb_each_text record SRFs (#155)
# --------------------------------------------------------------------------- #


def test_jsonb_each_from():
    rows = _rows('SELECT * FROM jsonb_each(\'{"a":1,"b":"x"}\'::jsonb) ORDER BY key')
    assert rows == [("a", 1), ("b", "x")]


def test_jsonb_each_columns():
    r = _run("SELECT * FROM jsonb_each('{\"a\":1}'::jsonb)")
    assert [(c.name, c.type_tag) for c in r.columns] == [("key", "text"), ("value", "json")]


def test_jsonb_each_text_stringifies_values():
    rows = _rows(
        'SELECT * FROM jsonb_each_text(\'{"a":1,"b":"x","c":{"d":2}}\'::jsonb) ORDER BY key'
    )
    assert rows == [("a", "1"), ("b", "x"), ("c", '{"d": 2}')]


def test_jsonb_each_text_columns_are_text():
    r = _run("SELECT * FROM jsonb_each_text('{\"a\":1}'::jsonb)")
    assert [(c.name, c.type_tag) for c in r.columns] == [("key", "text"), ("value", "text")]


def test_jsonb_each_column_aliases():
    assert _cols("SELECT * FROM jsonb_each('{\"a\":1}'::jsonb) AS t(k, v)") == ["k", "v"]
    assert _rows("SELECT k, v FROM jsonb_each('{\"a\":1}'::jsonb) AS t(k, v)") == [("a", 1)]


def test_jsonb_each_where_and_order():
    rows = _rows(
        'SELECT key FROM jsonb_each(\'{"a":1,"b":2,"c":3}\'::jsonb) '
        "WHERE value::int > 1 ORDER BY key"
    )
    assert rows == [("b",), ("c",)]


def test_jsonb_each_with_ordinality():
    r = _run("SELECT * FROM jsonb_each('{\"x\":9}'::jsonb) WITH ORDINALITY")
    assert [c.name for c in r.columns] == ["key", "value", "ordinality"]
    assert r.rows == [("x", 9, 1)]


def test_jsonb_each_empty_object():
    assert _rows("SELECT * FROM jsonb_each('{}'::jsonb)") == []


def test_json_each_text_bool_and_null():
    rows = _rows('SELECT * FROM json_each_text(\'{"a":true,"b":null}\'::json) ORDER BY key')
    assert rows == [("a", "true"), ("b", None)]


def test_computed_projection_over_srf():
    assert _rows("select x * 2 from generate_series(1, 3) as t(x)") == [(2,), (4,), (6,)]
    assert _rows("select x * 2 as y from generate_series(1, 3) as t(x) order by y desc") == [
        (6,),
        (4,),
        (2,),
    ]
    assert _rows("select 1 from generate_series(1, 3)") == [(1,), (1,), (1,)]


def test_computed_projection_over_catalog_table():
    st = Storage(":memory:")
    try:
        sess = Session(database=DB)
        run_sql(st, DB, "create table pc (a int4)", session=sess)
        sql = "select upper(relname) from pg_class where relname = 'pc'"
        res = run_sql(st, DB, sql, session=sess)[-1]
        assert res.rows == [("PC",)]
        res = run_sql(st, DB, "select 1 from pg_namespace limit 1", session=sess)[-1]
        assert res.rows == [(1,)]
    finally:
        st.close()


def test_generate_series_accepts_untyped_text_bounds():
    """A bound arriving as text is parsed as a number, as Postgres does.

    An untyped parameter (`generate_series(1, $1)` with `$1` sent without a
    type OID) reaches the SRF as a string — nothing upstream coerced it,
    because the wire gave no type. Postgres infers the parameter's type from
    the argument position and parses it as an integer.

    This is not academic: pgx's `ensureConnValid` helper runs exactly that
    query and is called at the end of 66 `pgconn` tests, so rejecting it took
    otherwise-passing tests down with it. Fixing it moved that package from 86
    failures to 29.
    """
    assert _rows("select generate_series(1, '3')") == [(1,), (2,), (3,)]
    assert _rows("select generate_series('1', '3')") == [(1,), (2,), (3,)]
    # A numeric step still works alongside coerced bounds.
    assert _rows("select generate_series('1', '10', 3)") == [(1,), (4,), (7,), (10,)]

    # NOT covered, deliberately: a QUOTED third argument
    # (`generate_series(1, 10, '3')`). sqlglot parses that into an `Interval`
    # node at parse time, so it arrives as an interval and never reaches this
    # coercion — a separate parser-level quirk, not this fix's job. Recorded
    # here rather than silently omitted.


def test_generate_series_still_rejects_non_numeric_bounds():
    """Coercion is narrow: only strings that parse cleanly become numbers.

    Guards the over-permissive direction — a bound that is genuinely not a
    number must still raise, not silently yield an empty or nonsense series.
    """
    import pytest as _pytest

    from secantus.sql import errors

    with _pytest.raises(errors.SQLError) as exc:
        _rows("select generate_series('a', 'b')")
    assert "integer / numeric" in str(exc.value)


class TestSeriesResultTyping:
    # pgx's NetworkUsage test byte-counts the reply: PG's generate_series
    # picks its overload from the argument types, so int4-range bounds yield
    # int4 rows (oid 23, 4-byte binary cells), not int8.
    def test_int4_bounds_type_int4(self):
        res = _run("select n from generate_series(1, 3) n")
        assert res.columns[0].type_tag == "int4"
        assert res.columns[0].pg_oid == 23

    def test_int8_bounds_type_int8(self):
        res = _run("select n from generate_series(2147483648, 2147483650) n")
        assert res.columns[0].type_tag == "int8"
        assert res.columns[0].pg_oid == 20

    def test_describe_matches_execute_for_param_bound(self):
        # Describe-time ($1 unbound) and execute-time typing must agree — a
        # RowDescription claiming int8 over 4-byte int4 cells breaks binary
        # clients. Known int4-range bounds decide int4 both times.
        from secantus.sql import srf as _srf

        assert _srf._generate_series(1, None, None) == ([], "int4")
        assert _srf._generate_series(2147483648, None, None) == ([], "int8")


class TestOutputNameFidelity:
    def test_duplicate_unaliased_names_repeat_verbatim(self):
        # Real PG repeats ?column? for every unaliased expression — never
        # ?column?_2 (pgx's NetworkUsage test byte-counts the names).
        assert _cols("select 'a', 'b', 1, 2 from generate_series(1, 1) n") == [
            "?column?",
            "?column?",
            "?column?",
            "?column?",
        ]

    def test_array_cast_named_after_element_type(self):
        assert _cols("select '{foo}'::text[] from generate_series(1, 1) n") == ["text"]


class TestSelectListRecordSrf:
    """``_pg_expandarray`` in the SELECT list — the call sites pgjdbc's
    DatabaseMetaData PK/index queries emit: bare composite column, immediate
    ``(SRF(x)).n`` field access, and both in one projection expanding in
    lockstep. Rows multiply per element; empty arrays eliminate the row."""

    @pytest.fixture
    def st(self, tmp_path):
        s = Storage(str(tmp_path))
        try:
            sess = Session(database=DB)
            run_sql(s, DB, "CREATE TABLE t (id int, arr int[])", session=sess)
            run_sql(
                s,
                DB,
                "INSERT INTO t VALUES (1, ARRAY[10,20]), (2, ARRAY[30]), (3, ARRAY[]::int[])",
                session=sess,
            )
            yield s
        finally:
            s.close()

    def test_bare_composite_column(self, st):
        res = _run(
            "SELECT id, information_schema._pg_expandarray(arr) AS keys FROM t ORDER BY id",
            st=st,
        )
        assert res.rows == [
            (1, {"x": 10, "n": 1}),
            (1, {"x": 20, "n": 2}),
            (2, {"x": 30, "n": 1}),
        ]
        assert [c.name for c in res.columns] == ["id", "keys"]

    def test_field_access_form(self, st):
        res = _run(
            "SELECT id, (information_schema._pg_expandarray(arr)).n AS seq FROM t ORDER BY id",
            st=st,
        )
        assert res.rows == [(1, 1), (1, 2), (2, 1)]

    def test_lockstep_expansion(self, st):
        res = _run(
            "SELECT (information_schema._pg_expandarray(arr)).n AS seq, "
            "information_schema._pg_expandarray(arr) AS keys FROM t WHERE id = 1",
            st=st,
        )
        assert res.rows == [(1, {"x": 10, "n": 1}), (2, {"x": 20, "n": 2})]

    def test_fromless(self, st):
        res = _run("SELECT information_schema._pg_expandarray(ARRAY[7,8]) AS k", st=st)
        assert res.rows == [({"x": 7, "n": 1},), ({"x": 8, "n": 2},)]

    def test_composite_access_through_derived_table(self, st):
        res = _run(
            "SELECT (sub.keys).x, (sub.keys).n FROM "
            "(SELECT information_schema._pg_expandarray(ARRAY[10,20]) AS keys) sub",
            st=st,
        )
        assert res.rows == [(10, 1), (20, 2)]

    def test_pgjdbc_get_primary_keys_shape(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE pkt2 (p int, q int, PRIMARY KEY (p, q))", session=sess)
        res = _run(
            "SELECT result.COLUMN_NAME, result.KEY_SEQ, result.PK_NAME FROM "
            "(SELECT a.attname AS COLUMN_NAME, "
            " (information_schema._pg_expandarray(con.conkey)).n AS KEY_SEQ, "
            " con.conname AS PK_NAME, "
            " information_schema._pg_expandarray(con.conkey) AS KEYS, "
            " a.attnum AS A_ATTNUM "
            "FROM pg_catalog.pg_constraint con "
            " JOIN pg_catalog.pg_class ct ON (con.conrelid = ct.oid) "
            " JOIN pg_catalog.pg_attribute a ON (a.attrelid = ct.oid) "
            "WHERE con.contype = 'p' AND ct.relname = 'pkt2') result "
            "where result.A_ATTNUM = (result.KEYS).x "
            "ORDER BY result.key_seq",
            st=st,
        )
        assert res.rows == [("p", 1, "pkt2_pkey"), ("q", 2, "pkt2_pkey")]

    def test_pgjdbc_index_keys_join_shape(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE pkt (a int, b text, PRIMARY KEY (a))", session=sess)
        res = _run(
            "SELECT a.attname FROM pg_catalog.pg_class ct "
            " JOIN pg_catalog.pg_attribute a ON (ct.oid = a.attrelid) "
            " JOIN (SELECT i.indexrelid, i.indrelid, i.indisprimary, "
            "        information_schema._pg_expandarray(i.indkey) AS keys "
            "       FROM pg_catalog.pg_index i) i "
            "   ON (a.attnum = (i.keys).x AND a.attrelid = i.indrelid) "
            "WHERE ct.relname = 'pkt' AND i.indisprimary ORDER BY a.attnum",
            st=st,
        )
        assert res.rows == [("a",)]


class TestSearchPathVisibility:
    """``pg_table_is_visible`` honours the session's search_path — both in
    WHERE position (lowered to a namespace filter) and as a projected value —
    so pgjdbc's getPrimaryUniqueKeys disambiguates same-named tables across
    schemas (UpdateableResultTest.testUpdateableWithSameTableNameInMultipleSchemas)."""

    @pytest.fixture
    def st(self, tmp_path):
        s = Storage(str(tmp_path))
        try:
            sess = Session(database=DB)
            for ddl in (
                "CREATE SCHEMA schema1",
                "CREATE SCHEMA schema2",
                "CREATE TABLE schema1.same_name (id int PRIMARY KEY, val text)",
                "CREATE TABLE schema2.same_name (id2 int PRIMARY KEY, val text)",
            ):
                run_sql(s, DB, ddl, session=sess)
            yield s
        finally:
            s.close()

    def _pk_columns(self, st, sess):
        q = (
            "SELECT a.attname FROM pg_catalog.pg_class ct "
            " JOIN pg_catalog.pg_attribute a ON (ct.oid = a.attrelid) "
            " JOIN pg_catalog.pg_index i ON (a.attrelid = i.indrelid) "
            " JOIN (SELECT i2.indrelid AS rid, "
            "        information_schema._pg_expandarray(i2.indkey) AS keys "
            "       FROM pg_catalog.pg_index i2) k "
            "   ON (a.attnum = (k.keys).x AND a.attrelid = k.rid) "
            "WHERE i.indisprimary AND pg_catalog.pg_table_is_visible(ct.oid) "
            "  AND ct.relname = 'same_name'"
        )
        return run_sql(st, DB, q, session=sess)[-1].rows

    def test_where_position_follows_search_path(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "SET search_path TO schema1", session=sess)
        assert self._pk_columns(st, sess) == [("id",)]
        run_sql(st, DB, "SET search_path TO schema2", session=sess)
        assert self._pk_columns(st, sess) == [("id2",)]

    def test_projection_position_follows_search_path(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "SET search_path TO schema1", session=sess)
        rows = run_sql(
            st,
            DB,
            "SELECT pg_catalog.pg_table_is_visible(oid) FROM pg_catalog.pg_class "
            "WHERE relname = 'same_name' ORDER BY relnamespace",
            session=sess,
        )[-1].rows
        assert rows == [(True,), (False,)]

    def test_default_path_still_sees_public(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE pub_t (a int PRIMARY KEY)", session=sess)
        rows = run_sql(
            st,
            DB,
            "SELECT relname FROM pg_catalog.pg_class "
            "WHERE pg_catalog.pg_table_is_visible(oid) AND relname = 'pub_t'",
            session=sess,
        )[-1].rows
        assert rows == [("pub_t",)]
