"""Base-column identity in RowDescription, and the CREATE VIEW column list.

Postgres reports each result column's source relation and its 1-based position
in that relation. pgtest's ``row_description`` corpus reads those two fields
byte-for-byte across a JOIN, through a VIEW, and after an ALTER COLUMN TYPE;
JDBC's updatable ResultSet resolves base columns through the same pair. The
padding and CREATE VIEW semantics here were probed against a real PostgreSQL 14.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def joined(storage, session):
    """``tab1(a, b)`` joined to ``tab2(c, tab1_a)`` — the corpus's shape, where
    the interesting columns (``a``, ``c``) are attnum 1 of *different* tables."""
    run(storage, session, "CREATE TABLE tab1 (a INT8 PRIMARY KEY, b INT8)")
    run(storage, session, "CREATE TABLE tab2 (c INT8 PRIMARY KEY, tab1_a INT8)")
    run(storage, session, "INSERT INTO tab1 VALUES (1, 2)")
    run(storage, session, "INSERT INTO tab2 VALUES (4, 1)")
    return storage, session


def identity(res):
    return [(c.name, c.attnum) for c in res.columns]


def test_join_columns_keep_their_own_tables_attnum(joined):
    storage, session = joined
    res = run(
        storage,
        session,
        "SELECT tab1.a, tab2.c FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a",
    )
    # Both are the first column OF THEIR OWN table, so both are attnum 1 —
    # a single base-table map would have had to call one of them 0.
    assert identity(res) == [("a", 1), ("c", 1)]
    assert all(c.table_oid for c in res.columns)
    assert res.columns[0].table_oid != res.columns[1].table_oid


def test_join_columns_report_the_position_within_their_table(joined):
    storage, session = joined
    res = run(
        storage,
        session,
        "SELECT tab1.b, tab2.tab1_a FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a",
    )
    assert identity(res) == [("b", 2), ("tab1_a", 2)]


def test_an_unqualified_join_column_resolves_to_the_table_holding_it(joined):
    storage, session = joined
    res = run(storage, session, "SELECT b, c FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a")
    assert identity(res) == [("b", 2), ("c", 1)]


def test_a_computed_join_output_has_no_base_column(joined):
    storage, session = joined
    res = run(
        storage,
        session,
        "SELECT tab1.a, tab1.a + tab2.c AS total FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a",
    )
    assert identity(res) == [("a", 1), ("total", 0)]
    assert res.columns[1].table_oid == 0


def test_star_over_a_join_attributes_every_column(joined):
    storage, session = joined
    res = run(storage, session, "SELECT * FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a")
    assert identity(res) == [("a", 1), ("b", 2), ("c", 1), ("tab1_a", 2)]


class TestViewColumnList:
    """``CREATE VIEW v (v1, v2) AS …`` — the declared names parse as a Schema
    node wrapping the view name, which used to leave the name empty: the view
    was filed under "" while CREATE VIEW still reported success."""

    def test_a_view_with_a_column_list_is_queryable(self, joined):
        storage, session = joined
        run(
            storage,
            session,
            "CREATE VIEW v (v1, v2) AS "
            "SELECT a, tab1_a FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a",
        )
        res = run(storage, session, "SELECT * FROM v WHERE v1 = 1")
        assert [c.name for c in res.columns] == ["v1", "v2"]
        assert res.rows == [(1, 1)]

    def test_view_columns_report_the_views_own_positions(self, joined):
        storage, session = joined
        run(
            storage,
            session,
            "CREATE VIEW v (v1, v2) AS "
            "SELECT a, tab1_a FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a",
        )
        res = run(storage, session, "SELECT * FROM v")
        # The view is a relation of its own: positions 1 and 2 within the VIEW,
        # not the underlying tables', and one shared (view) relation oid.
        assert identity(res) == [("v1", 1), ("v2", 2)]
        assert res.columns[0].table_oid == res.columns[1].table_oid != 0

    def test_selecting_a_view_column_by_name_keeps_its_position(self, joined):
        storage, session = joined
        run(
            storage,
            session,
            "CREATE VIEW v (v1, v2) AS "
            "SELECT a, tab1_a FROM tab1 JOIN tab2 ON tab1.a = tab2.tab1_a",
        )
        res = run(storage, session, "SELECT v2 FROM v")
        assert identity(res) == [("v2", 2)]

    def test_fewer_names_than_columns_leaves_the_rest_alone(self, storage, session):
        # Probed against PostgreSQL 14: CREATE VIEW vfew (x) AS SELECT a, b
        # renders as SELECT a AS x, b — surplus outputs keep their own names.
        run(storage, session, "CREATE TABLE t (a INT8, b INT8)")
        run(storage, session, "CREATE VIEW vfew (x) AS SELECT a, b FROM t")
        res = run(storage, session, "SELECT * FROM vfew")
        assert [c.name for c in res.columns] == ["x", "b"]

    def test_more_names_than_columns_is_an_error(self, storage, session):
        run(storage, session, "CREATE TABLE t (a INT8, b INT8)")
        with pytest.raises(SQLError) as exc:
            run(storage, session, "CREATE VIEW vmore (x, y, z) AS SELECT a, b FROM t")
        # PostgreSQL 14's wording and SQLSTATE, verbatim.
        assert exc.value.sqlstate == "42601"
        assert "more column names than columns" in str(exc.value)

    def test_a_star_body_resolves_its_columns_at_creation(self, storage, session):
        run(storage, session, "CREATE TABLE t (a INT8, b INT8)")
        run(storage, session, "CREATE VIEW vstar (v1, v2) AS SELECT * FROM t")
        run(storage, session, "INSERT INTO t VALUES (1, 2)")
        res = run(storage, session, "SELECT * FROM vstar")
        assert [c.name for c in res.columns] == ["v1", "v2"]
        assert res.rows == [(1, 2)]

    def test_a_star_body_ignores_tables_named_only_in_a_subquery(self, storage, session):
        # The star stands for the statement's OWN sources: a table referenced
        # from a subquery in the WHERE must not contribute columns to the view.
        run(storage, session, "CREATE TABLE t (a INT8, b INT8)")
        run(storage, session, "CREATE TABLE u (y INT8)")
        run(storage, session, "INSERT INTO t VALUES (1, 2)")
        run(storage, session, "INSERT INTO u VALUES (1)")
        run(
            storage,
            session,
            "CREATE VIEW vsub (p, q) AS SELECT * FROM t WHERE a IN (SELECT y FROM u)",
        )
        res = run(storage, session, "SELECT * FROM vsub")
        assert [c.name for c in res.columns] == ["p", "q"]
        assert res.rows == [(1, 2)]

    def test_a_star_view_freezes_its_column_list(self, storage, session):
        # PostgreSQL resolves the star once, at creation: a column added to the
        # table afterwards does NOT appear in the view (probed against 14).
        run(storage, session, "CREATE TABLE t (a INT8, b INT8)")
        run(storage, session, "CREATE VIEW vstar (v1, v2) AS SELECT * FROM t")
        run(storage, session, "ALTER TABLE t ADD COLUMN newcol INT8")
        res = run(storage, session, "SELECT * FROM vstar")
        assert [c.name for c in res.columns] == ["v1", "v2"]


class TestBlankPadding:
    """``char(n)`` is a blank-padded type: the value goes out padded to n, while
    the semantics that read it (length, comparison, cast to text) see it
    unpadded. Both halves probed against PostgreSQL 14."""

    def test_a_char_column_pads_on_the_wire(self, storage, session):
        from secantus.sql import typemap

        run(storage, session, "CREATE TABLE t3 (a INT8 PRIMARY KEY, b CHAR(8))")
        run(storage, session, "INSERT INTO t3 VALUES (4, 'hello')")
        res = run(storage, session, "SELECT b FROM t3")
        col = res.columns[0]
        assert (col.pg_oid, col.typmod) == (1042, 12)
        assert typemap.blank_pad(res.rows[0][0], col.pg_oid, col.typmod) == "hello   "

    def test_length_ignores_the_padding(self, storage, session):
        run(storage, session, "CREATE TABLE t3 (a INT8 PRIMARY KEY, b CHAR(8))")
        run(storage, session, "INSERT INTO t3 VALUES (4, 'hello')")
        # PostgreSQL 14: length('hello'::char(8)) is 5, not 8.
        assert run(storage, session, "SELECT length(b) FROM t3").rows == [(5,)]

    def test_padding_never_truncates_or_touches_other_types(self):
        from secantus.sql import typemap

        assert typemap.blank_pad("exactly8", 1042, 12) == "exactly8"
        assert typemap.blank_pad("hi", 1042, -1) == "hi"  # bare char has no width
        assert typemap.blank_pad("hi", 1043, 12) == "hi"  # varchar is not padded
        assert typemap.blank_pad(None, 1042, 12) is None
        assert typemap.blank_pad(7, 1042, 12) == 7


def test_alter_column_type_replaces_the_declared_identity(storage, session):
    run(storage, session, "CREATE TABLE tab3 (a INT8 PRIMARY KEY, b CHAR(8))")
    run(storage, session, "INSERT INTO tab3 VALUES (4, 'hello')")
    before = run(storage, session, "SELECT b FROM tab3").columns[0]
    assert (before.pg_oid, before.typmod) == (1042, 12)

    run(storage, session, "ALTER TABLE tab3 ALTER COLUMN b TYPE TEXT")
    after = run(storage, session, "SELECT b FROM tab3").columns[0]
    # The old declaration must not survive the retype — it kept reporting
    # bpchar/12 for a text column, and with it the phantom blank padding.
    assert (after.pg_oid, after.typmod) == (25, -1)
    assert after.attnum == 2  # position is stable across the retype


def test_alter_column_type_to_a_sized_type_takes_the_new_typmod(storage, session):
    run(storage, session, "CREATE TABLE t (a INT8, b TEXT)")
    run(storage, session, "ALTER TABLE t ALTER COLUMN b TYPE VARCHAR(10)")
    col = run(storage, session, "SELECT b FROM t").columns[0]
    assert (col.pg_oid, col.typmod) == (1043, 14)
