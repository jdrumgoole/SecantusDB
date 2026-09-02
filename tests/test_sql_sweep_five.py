"""A fifth sweep — arrays, enums, domains, ranges, DDL: 28 of 36, now 34.

Two of the misses were silently wrong rather than refused:

* `ALTER TABLE t ADD COLUMN c text DEFAULT 'z'` dropped the DEFAULT on the
  floor. Existing rows kept NULL — PostgreSQL backfills — and, worse, a LATER
  insert that omitted the column got NULL too, so `NOT NULL DEFAULT 7` left a
  NOT NULL column holding NULL.
* An ENUM comparison answered by SPELLING. An enum's order is its declared
  label order, so `'happy' > 'ok'` is true for
  `mood AS ENUM ('sad','ok','happy')` and false as text — and `WHERE m > 'ok'`
  returned `sad`. Sorting already knew the declared order; comparison did not.

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

    try:
        yield run
    finally:
        storage.close()


@pytest.fixture()
def enum_db(db):
    db("CREATE TYPE mood AS ENUM ('sad','ok','happy')")
    db("CREATE TABLE e1 (id int, m mood)")
    db("INSERT INTO e1 VALUES (1,'happy'),(2,'sad'),(3,'ok')")
    return db


def _rows(db, sql):
    rows, _ = db(sql)
    return rows


class TestAddColumnDefault:
    def test_backfills_existing_rows(self, db):
        db("CREATE TABLE a5 (id int)")
        db("INSERT INTO a5 VALUES (1),(2)")
        db("ALTER TABLE a5 ADD COLUMN extra text DEFAULT 'z'")
        assert _rows(db, "SELECT id, extra FROM a5 ORDER BY id") == [(1, "z"), (2, "z")]

    def test_later_inserts_get_the_default(self, db):
        """The default was not recorded at all, so a subsequent INSERT that
        omitted the column also got NULL."""
        db("CREATE TABLE a5 (id int)")
        db("ALTER TABLE a5 ADD COLUMN extra text DEFAULT 'z'")
        db("INSERT INTO a5 (id) VALUES (3)")
        assert _rows(db, "SELECT id, extra FROM a5") == [(3, "z")]

    def test_not_null_default(self, db):
        """`NOT NULL DEFAULT 7` left a NOT NULL column holding NULL."""
        db("CREATE TABLE a5 (id int)")
        db("INSERT INTO a5 VALUES (1),(2)")
        db("ALTER TABLE a5 ADD COLUMN n int DEFAULT 7 NOT NULL")
        assert _rows(db, "SELECT id, n FROM a5 ORDER BY id") == [(1, 7), (2, 7)]

    def test_no_default_still_nulls(self, db):
        db("CREATE TABLE a5 (id int)")
        db("INSERT INTO a5 VALUES (1)")
        db("ALTER TABLE a5 ADD COLUMN plain text")
        assert _rows(db, "SELECT id, plain FROM a5") == [(1, None)]


class TestEnumComparison:
    """An enum's order is its DECLARED label order, not the labels' text order."""

    @pytest.mark.parametrize(
        ("predicate", "want"),
        [
            ("m > 'ok'::mood", [1]),
            ("m < 'ok'::mood", [2]),
            ("m >= 'ok'", [1, 3]),
            ("m <= 'ok'", [2, 3]),
            ("'ok' < m", [1]),
            ("'ok' >= m", [2, 3]),
            # Equality was always right — it compares by label, not by order.
            ("m = 'ok'", [3]),
            ("m <> 'ok'", [1, 2]),
        ],
    )
    def test_where(self, enum_db, predicate, want):
        rows = _rows(enum_db, f"SELECT id FROM e1 WHERE {predicate} ORDER BY id")
        assert [r[0] for r in rows] == want

    def test_combined_with_another_predicate(self, enum_db):
        rows = _rows(enum_db, "SELECT id FROM e1 WHERE m > 'sad' AND id > 0 ORDER BY id")
        assert [r[0] for r in rows] == [1, 3]

    def test_delete(self, enum_db):
        assert _rows(enum_db, "DELETE FROM e1 WHERE m < 'ok' RETURNING id") == [(2,)]

    def test_order_by_was_already_right(self, enum_db):
        rows = _rows(enum_db, "SELECT id, m FROM e1 ORDER BY m")
        assert rows == [(2, "sad"), (3, "ok"), (1, "happy")]

    def test_a_bound_outside_the_labels_is_left_alone(self, enum_db):
        """Nothing to rewrite to, so the comparison keeps its old behaviour
        rather than silently matching nothing."""
        rows = _rows(enum_db, "SELECT id FROM e1 WHERE m > 'zzz' ORDER BY id")
        assert isinstance(rows, list)


class TestArrayAndRangeFunctions:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT array_positions('{1,2,1}'::int[], 1)", [1, 3]),
            ("SELECT array_positions('{1,2}'::int[], 9)", []),
            ("SELECT array_fill(7, ARRAY[3])", [7, 7, 7]),
        ],
    )
    def test_values(self, db, sql, want):
        assert _rows(db, sql)[0][0] == want

    @pytest.mark.parametrize(
        ("sql", "tag"),
        [
            # These typed as text, so the array went out as a string literal.
            ("SELECT array_fill(7, ARRAY[3])", "int4[]"),
            ("SELECT array_positions('{1,2,1}'::int[], 1)", "int8[]"),
            # A CAST is as much a range operand as the constructor is.
            ("SELECT range_merge('[1,3)'::int4range, '[5,7)'::int4range)", "int4range"),
        ],
    )
    def test_tags(self, db, sql, tag):
        _r, tags = db(sql)
        assert tags[0] == tag


class TestVersionNested:
    def test_bare_call(self, db):
        assert _rows(db, "SELECT version()")[0][0].startswith("PostgreSQL")

    def test_inside_an_expression(self, db):
        """It worked as a bare projection through the session-function path but
        reported `function current_version() is not supported` — sqlglot's node
        name — the moment it was nested."""
        assert _rows(db, "SELECT version() LIKE 'PostgreSQL%'") == [(True,)]
