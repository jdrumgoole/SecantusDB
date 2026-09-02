"""The jsonb function family: four no-ops and a family of wrong renderings.

`jsonb_set('{"a":1}','{b}','2')` returned its input unchanged. So did
`jsonb_strip_nulls`. Both are implemented — they only ever worked when the
argument carried an explicit `::jsonb` cast. A bare `'{"a":1}'` literal is
PostgreSQL's `unknown`, and there the function's declared parameter type
resolves it; here it stayed a Python `str`, the navigation had nothing to walk,
and the call returned the input. A no-op that looks like a success.

`jsonb_build_array(1,'x',true)::text` rendered the PostgreSQL array `{1,x,t}`
instead of the JSON `[1, "x", true]`, and `to_jsonb('x'::text)::text` a bare
`x` instead of `"x"`. Their VALUES are ordinary Python lists, dicts and
strings, so only the CALL says the rendering should be JSON.

`jsonb_object_keys` yielded insertion order where PostgreSQL yields storage
order — shorter keys first, then bytewise. `json_object_keys`, which keeps the
input's own order, was right as it was.

`jsonb_typeof(v->'arr')` was `0A000 unsupported scalar expression`: inside a
function call, `v -> 'arr'` looks like an arrow-LAMBDA to sqlglot's parser and
only reaches `JSONExtract` when the left side is something an identifier cannot
be. PostgreSQL has no lambda syntax, so a lambda here is always that misparse.

Every expectation here was measured against PostgreSQL 14.13.
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

    run("CREATE TABLE j (i int, v jsonb)")
    run("""INSERT INTO j VALUES (1,'{"k":1,"arr":[1,2]}'), (2,'{"k":2}'), (3,NULL)""")
    run("CREATE TABLE ja (i int, t text)")
    run("INSERT INTO ja VALUES (1,'a'),(2,'b'),(3,'c')")
    try:
        yield run
    finally:
        storage.close()


def _one(db, sql):
    rows, _ = db(sql)
    return rows[0][0]


class TestUntypedArgumentIsCoerced:
    """Each of these was a no-op — or an unchanged input — without a cast."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("""SELECT jsonb_set('{"a":1}','{b}','2')::text""", '{"a": 1, "b": 2}'),
            ("""SELECT jsonb_insert('{"a":1}','{b}','2')::text""", '{"a": 1, "b": 2}'),
            ("""SELECT jsonb_strip_nulls('{"a":null,"b":1}')::text""", '{"b": 1}'),
            ("""SELECT jsonb_typeof('{"a":1}')""", "object"),
            ("""SELECT jsonb_typeof('[1]')""", "array"),
            ("""SELECT jsonb_pretty('{"a":1}')""", '{\n    "a": 1\n}'),
        ],
    )
    def test_untyped_literal(self, db, sql, want):
        assert _one(db, sql) == want

    def test_cast_spelling_still_works(self, db):
        assert (
            _one(db, """SELECT jsonb_set('{"a":1}'::jsonb,'{b}','2'::jsonb)::text""")
            == '{"a": 1, "b": 2}'
        )


class TestJsonRendering:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT jsonb_build_array(1,'x',true)::text", '[1, "x", true]'),
            ("""SELECT jsonb_build_object('a',1,'b','x')::text""", '{"a": 1, "b": "x"}'),
            ("SELECT to_jsonb(1)::text", "1"),
            # A jsonb STRING renders quoted.
            ("SELECT to_jsonb('x'::text)::text", '"x"'),
            ("SELECT to_jsonb(ARRAY[1,2])::text", "[1, 2]"),
        ],
    )
    def test_renders_as_json(self, db, sql, want):
        assert _one(db, sql) == want

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT json_agg(i)::text FROM ja", "[1, 2, 3]"),
            ("SELECT json_agg(i ORDER BY i)::text FROM ja", "[1, 2, 3]"),
        ],
    )
    def test_json_aggregate(self, db, sql, want):
        """By the time the cast is evaluated its operand is a synthetic column,
        so the call is no longer visible — the planner marks it instead."""
        assert _one(db, sql) == want

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT array_agg(i)::text FROM ja", "{1,2,3}"),
            ("SELECT array_agg(t ORDER BY t DESC)::text FROM ja", "{c,b,a}"),
            ("SELECT ARRAY[1,2]::text", "{1,2}"),
            ("SELECT 'plain'::text", "plain"),
        ],
    )
    def test_arrays_and_text_are_unaffected(self, db, sql, want):
        assert _one(db, sql) == want


class TestObjectKeyOrder:
    def test_jsonb_uses_storage_order(self, db):
        """Shorter keys first, then bytewise — not insertion order."""
        rows, _ = db("""SELECT jsonb_object_keys('{"c":1,"aa":2,"b":3,"zz":4}')""")
        assert [r[0] for r in rows] == ["b", "c", "aa", "zz"]

    def test_json_keeps_input_order(self, db):
        rows, _ = db("""SELECT json_object_keys('{"c":1,"aa":2,"b":3}')""")
        assert [r[0] for r in rows] == ["c", "aa", "b"]


class TestArrowInsideACall:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT jsonb_typeof(v->'arr') FROM j WHERE i = 1", "array"),
            # The row without that key gets NULL, not an error.
            ("SELECT jsonb_typeof(v->'arr') FROM j WHERE i = 2", None),
            ("SELECT jsonb_array_length(v->'arr') FROM j WHERE i = 1", 2),
            ("SELECT jsonb_array_length(v->'arr') FROM j WHERE i = 2", None),
        ],
    )
    def test_arrow_argument(self, db, sql, want):
        assert _one(db, sql) == want

    def test_arrow_alone_was_always_right(self, db):
        assert _one(db, "SELECT v->'arr' FROM j WHERE i = 1") == [1, 2]


class TestToCharIsoTokens:
    @pytest.mark.parametrize(
        ("fmt", "want"),
        [
            ("IYYY-IW-ID", "2020-01-3"),
            ("IYYY", "2020"),
            ("IW", "01"),
            ("ID", "3"),
            # `MI` contains an `I` and must not be caught by the ISO tokens.
            ("HH24:MI:SS", "10:30:45"),
            ("MI", "30"),
            # The word tokens keep their padding and casing.
            ("Day", "Wednesday"),
            ("Month", "January  "),
            ("YYYY-MM-DD", "2020-01-01"),
        ],
    )
    def test_tokens(self, db, fmt, want):
        assert _one(db, f"SELECT to_char('2020-01-01 10:30:45'::timestamp, '{fmt}')") == want

    def test_mid_year_iso_week(self, db):
        assert _one(db, "SELECT to_char('2020-06-15'::date, 'IYYY-IW-ID')") == "2020-25-1"
