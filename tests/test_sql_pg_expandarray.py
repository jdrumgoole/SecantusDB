"""``information_schema._pg_expandarray`` in the select list.

The function yields one ``(x, n)`` record per array element — the value and its
1-based subscript. pgjdbc's ``DatabaseMetaData.getPrimaryKeys`` /
``getIndexInfo`` select it two ways in the same query: the whole record, and a
single field via ``(…).n``. Neither call shape was recognised, so those
metadata queries failed outright — the largest remaining cluster in the pgjdbc
gauge, and what ``UpdateableResultTest`` needs to identify a row to update.
"""

from __future__ import annotations

import decimal

import pytest

from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

# The shape pgjdbc emits for getPrimaryKeys: the record selected whole *and*
# by field, over a five-way catalog join.
PRIMARY_KEYS_SQL = (
    "SELECT n.nspname AS table_schem, ct.relname AS table_name, "
    "  a.attname AS column_name, "
    "  (information_schema._pg_expandarray(i.indkey)).n AS key_seq, "
    "  ci.relname AS pk_name, "
    "  information_schema._pg_expandarray(i.indkey) AS keys, "
    "  a.attnum AS a_attnum "
    "FROM pg_catalog.pg_class ct "
    "  JOIN pg_catalog.pg_attribute a ON (ct.oid = a.attrelid) "
    "  JOIN pg_catalog.pg_namespace n ON (ct.relnamespace = n.oid) "
    "  JOIN pg_catalog.pg_index i ON (a.attrelid = i.indrelid) "
    "  JOIN pg_catalog.pg_class ci ON (ci.oid = i.indexrelid) "
    "WHERE i.indisprimary AND ct.relname = 'pkt'"
)


@pytest.fixture()
def q(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE src (id int primary key)")
    run("INSERT INTO src VALUES (1)")
    try:
        yield run
    finally:
        storage.close()


class TestFieldSelection:
    def test_subscript_field(self, q):
        assert q("SELECT (information_schema._pg_expandarray(ARRAY[7, 8])).n FROM src") == [
            (1,),
            (2,),
        ]

    def test_value_field(self, q):
        assert q("SELECT (information_schema._pg_expandarray(ARRAY[7, 8])).x FROM src") == [
            (7,),
            (8,),
        ]

    def test_unqualified_name(self, q):
        assert q("SELECT (_pg_expandarray(ARRAY[7, 8])).n FROM src") == [(1,), (2,)]

    def test_whole_record_stays_a_composite(self, q):
        """The record is kept as a subdocument, not flattened to Postgres'
        ``(7,1)`` composite text, because field access has to still work a
        level up — pgjdbc selects the record into a subquery column and then
        reads ``(result.keys).x`` from the outer query. Selecting the whole
        record straight to a client is a known gap (tasks/backlog.md): it is
        typed from its element and renders as JSON rather than ``(7,1)``.
        """
        assert q("SELECT information_schema._pg_expandarray(ARRAY[7, 8]) FROM src") == [
            ({"x": 7, "n": 1},),
            ({"x": 8, "n": 2},),
        ]

    def test_field_access_on_a_subquery_column(self, q):
        """The shape pgjdbc's getPrimaryKeys actually depends on: the record is
        produced in a subquery and a field is read from it in the outer WHERE."""
        inner = "SELECT id, information_schema._pg_expandarray(ARRAY[9, 8, 7]) AS keys FROM src"
        assert q(f"SELECT (result.keys).n FROM ({inner}) result WHERE (result.keys).x = 8") == [
            (2,)
        ]

    def test_alongside_an_ordinary_column(self, q):
        assert q("SELECT id, (information_schema._pg_expandarray(ARRAY[9, 8, 7])).x FROM src") == [
            (1, 9),
            (1, 8),
            (1, 7),
        ]

    def test_order_by_does_not_yet_sort_record_srf_rows(self, q):
        """STILL OPEN, and narrower than it was.

        `ORDER BY` over a plain `unnest` now sorts the expanded rows (see
        `TestSrfOrdering`), but the *record*-SRF field form —
        `(information_schema._pg_expandarray(arr)).x` — takes a different
        planning route that the fix does not reach, so it keeps array order.
        Postgres returns 7, 8, 9 here. Ordering by an alias on that form is
        worse: `(...).n AS n ... ORDER BY n` answers 42703, which predates this
        change (verified against the unpatched tree).

        Pinned at the CURRENT behaviour so the remaining gap stays visible —
        and named so it cannot be mistaken for the intended one.
        """
        assert q(
            "SELECT (information_schema._pg_expandarray(ARRAY[9, 8, 7])).x FROM src ORDER BY 1"
        ) == [(9,), (8,), (7,)]

    def test_empty_array_yields_no_rows(self, q):
        assert q("SELECT (information_schema._pg_expandarray(ARRAY[]::int[])).n FROM src") == []


class TestTheDriverQuery:
    def test_get_primary_keys_shape_runs(self, q):
        q("CREATE TABLE pkt (a int primary key, b int)")
        rows = q(PRIMARY_KEYS_SQL)
        assert rows, "the metadata query returned no rows"
        by_column = {r[2]: r for r in rows}
        assert "a" in by_column
        # key_seq is the 1-based position within the index key.
        assert by_column["a"][3] == 1
        assert by_column["a"][4] == "pkt_pkey"
        # The whole record is carried as a composite, so the outer query can
        # still read a field off it.
        assert by_column["a"][5] == {"x": 1, "n": 1}


class TestSrfElementTypes:
    """`unnest` declares the ARRAY'S ELEMENT type, not a hardcoded int4.

    The declared type used to be `int4` for every array, which is a wire lie for
    anything else: the RowDescription said int4 and the server then sent `a` /
    `1.5` / `t`, so a strict client did `int('a')` and raised
    `ValueError: invalid literal for int() with base 10: 'a'` — client-side, in
    psycopg, before the application ever saw a row. Only integer arrays worked,
    and only by luck. Verified against a live PostgreSQL 14.
    """

    def test_text_array_elements_are_text(self, q):
        assert q("SELECT unnest(ARRAY['a', 'b']) FROM src") == [("a",), ("b",)]

    def test_numeric_array_elements_are_numeric(self, q):
        assert q("SELECT unnest(ARRAY[1.5, 2.5]) FROM src") == [
            (decimal.Decimal("1.5"),),
            (decimal.Decimal("2.5"),),
        ]

    def test_bool_array_elements_are_bool(self, q):
        assert q("SELECT unnest(ARRAY[true, false]) FROM src") == [(True,), (False,)]

    def test_int_array_still_works(self, q):
        assert q("SELECT unnest(ARRAY[9, 8]) FROM src") == [(9,), (8,)]

    def test_a_subscript_is_an_int_whatever_the_array_holds(self, q):
        # generate_subscripts and `.n` yield the position, not an element.
        assert q("SELECT generate_subscripts(ARRAY['a', 'b'], 1) FROM src") == [(1,), (2,)]
        assert q("SELECT (information_schema._pg_expandarray(ARRAY['a', 'b'])).n FROM src") == [
            (1,),
            (2,),
        ]


class TestSrfOrdering:
    """`ORDER BY` over `unnest`, by ordinal and by output alias.

    Ordering by the alias — the form a real query uses — raised `0A000`
    outright; ordering by ordinal silently returned array order. Both are the
    same defect: the sort key has to come from the expanded row.
    PG-probed 14.
    """

    def test_order_by_ordinal(self, q):
        assert q("SELECT unnest(ARRAY[9,8,7]) AS u FROM src ORDER BY 1") == [(7,), (8,), (9,)]

    def test_order_by_output_alias(self, q):
        assert q("SELECT unnest(ARRAY[9,8,7]) AS u FROM src ORDER BY u") == [(7,), (8,), (9,)]

    def test_order_by_alias_descending(self, q):
        assert q("SELECT unnest(ARRAY[7,9,8]) AS u FROM src ORDER BY u DESC") == [
            (9,),
            (8,),
            (7,),
        ]

    def test_text_elements_sort_as_text(self, q):
        assert q("SELECT unnest(ARRAY['c','a','b']) AS u FROM src ORDER BY 1") == [
            ("a",),
            ("b",),
            ("c",),
        ]

    def test_without_order_by_array_order_is_kept(self, q):
        assert q("SELECT unnest(ARRAY[9,8,7]) FROM src") == [(9,), (8,), (7,)]
