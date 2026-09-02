"""Two more internal errors, plus three wrong answers, from a second sweep.

- `(1,2) < (1,3)` was `XX000`. A record rides as a dict of `f1..fN`, and a
  dict has no `<`. Equality worked, so only the ORDERING comparisons failed.
- `FETCH FIRST 2 ROWS ONLY` — the SQL-standard spelling of `LIMIT` that
  PostgreSQL accepts — was `XX000` twice over: it parses as `exp.Fetch`, whose
  count lives in `count` rather than `expression`, and the "unsupported" error
  built from that None then raised `AttributeError` on `None.sql()`. So the
  query could not even say why it failed.
- `3 BETWEEN SYMMETRIC 5 AND 1` answered FALSE. The keyword was parsed and
  ignored, so every reversed-bound test was wrong.
- `jsonb ? 'k'` and its `?|` / `?&` siblings reported
  `function jsonb_contains() is not supported` in a SELECT list — a name the
  user never wrote, mangled out of the node class for the two-key forms. They
  worked inside a WHERE all along.
- Every containment and key-existence operator typed as `text`, so a driver
  was sent `'t'` under oid 25 where PostgreSQL sends a boolean under oid 16.

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

    run("CREATE TABLE s2 (i int, v jsonb)")
    run("""INSERT INTO s2 VALUES (1,'{"k":1,"arr":[1,2]}'), (2,'{"k":2}'), (3,NULL)""")
    try:
        yield run
    finally:
        storage.close()


def _one(db, sql):
    rows, _ = db(sql)
    return rows[0][0]


class TestRecordComparison:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT (1,2) < (1,3)", True),
            ("SELECT (1,2) > (1,3)", False),
            ("SELECT (1,2) <= (1,2)", True),
            ("SELECT (2,1) > (1,9)", True),
            ("SELECT ROW(1,'a') < ROW(1,'b')", True),
            # Equality never went through the broken path.
            ("SELECT (1,2) = (1,2)", True),
            ("SELECT ROW(1,'a') = ROW(1,'a')", True),
        ],
    )
    def test_field_by_field(self, db, sql, want):
        assert _one(db, sql) is want


class TestFetchFirst:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT i FROM s2 ORDER BY i FETCH FIRST 2 ROWS ONLY", [1, 2]),
            ("SELECT i FROM s2 ORDER BY i FETCH FIRST 1 ROW ONLY", [1]),
            ("SELECT i FROM s2 ORDER BY i OFFSET 1 FETCH FIRST 1 ROW ONLY", [2]),
            # The LIMIT spelling was never affected.
            ("SELECT i FROM s2 ORDER BY i LIMIT 2", [1, 2]),
        ],
    )
    def test_fetch(self, db, sql, want):
        rows, _ = db(sql)
        assert [r[0] for r in rows] == want


class TestBetweenSymmetric:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # The bounds are put in order first.
            ("SELECT 3 BETWEEN SYMMETRIC 5 AND 1", True),
            ("SELECT 3 BETWEEN SYMMETRIC 1 AND 5", True),
            ("SELECT 7 BETWEEN SYMMETRIC 5 AND 1", False),
            ("SELECT 1 BETWEEN SYMMETRIC 1 AND 1", True),
            # Plain BETWEEN does NOT reorder.
            ("SELECT 3 BETWEEN 5 AND 1", False),
            ("SELECT 3 BETWEEN 1 AND 5", True),
        ],
    )
    def test_symmetric(self, db, sql, want):
        assert _one(db, sql) is want

    def test_null_bound(self, db):
        assert _one(db, "SELECT 3 BETWEEN SYMMETRIC NULL AND 5") is None


class TestJsonbExistence:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("""SELECT '{"k":1}'::jsonb ? 'k'""", True),
            ("""SELECT '{"k":1}'::jsonb ? 'z'""", False),
            # An ARRAY is asked about its string elements, a STRING about
            # equality — not just an object-key lookup.
            ("""SELECT '["a","b"]'::jsonb ? 'a'""", True),
            ("""SELECT '"x"'::jsonb ? 'x'""", True),
            ("""SELECT '1'::jsonb ? '1'""", False),
            ("""SELECT '{"a":1,"b":2}'::jsonb ?| ARRAY['b','z']""", True),
            ("""SELECT '{"a":1}'::jsonb ?| ARRAY['y','z']""", False),
            ("""SELECT '{"a":1,"b":2}'::jsonb ?& ARRAY['a','b']""", True),
            ("""SELECT '{"a":1}'::jsonb ?& ARRAY['a','z']""", False),
        ],
    )
    def test_exists(self, db, sql, want):
        assert _one(db, sql) is want

    def test_null_is_null(self, db):
        assert _one(db, "SELECT NULL::jsonb ? 'k'") is None

    def test_over_a_column(self, db):
        rows, _ = db("SELECT i, v ? 'k' FROM s2 ORDER BY i")
        assert rows == [(1, True), (2, True), (3, None)]


class TestBooleanOperatorTypes:
    """Each of these typed as `text`, so the value rode the wire as `'t'`."""

    @pytest.mark.parametrize(
        "sql",
        [
            """SELECT '{"k":1}'::jsonb @> '{"k":1}'::jsonb""",
            """SELECT '{"k":1}'::jsonb <@ '{"k":1,"z":2}'::jsonb""",
            """SELECT '{"k":1}'::jsonb ? 'k'""",
            """SELECT '{"a":1}'::jsonb ?| ARRAY['a']""",
            """SELECT '{"a":1}'::jsonb ?& ARRAY['a']""",
            "SELECT ARRAY[1,2] && ARRAY[2,3]",
        ],
    )
    def test_tag_is_bool(self, db, sql):
        _rows, tags = db(sql)
        assert tags[0] == "bool"
