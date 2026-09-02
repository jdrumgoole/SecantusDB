"""A nested `array_agg` silently dropped its in-call ORDER BY.

`SELECT array_agg(i ORDER BY i DESC) FROM t` sorted. Wrap it in anything at all
— a cast, an operator, a subscript, another function — and it returned
INSERTION order instead, with no error. `array_to_string(array_agg(x ORDER BY
y), ',')` is the shape that makes this look like ordinary SQL and get a wrong
answer.

The top-level projection path pushed `{v, k}` pairs and sorted them in a
post-aggregate step; the registrar for an aggregate nested inside a computed
projection registered a plain `$push` and never carried the ordering at all.
`EvaluatedSelectPlan` had no `post_aggregates` field to carry it through.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE a2 (i int, g int, s text)")
    run("CREATE TABLE b2 (i int, v text)")
    run("INSERT INTO a2 VALUES (1,1,'x'),(2,1,'y'),(3,2,'z'),(4,2,'w')")
    run("INSERT INTO b2 VALUES (1,'p'),(2,'q'),(3,'r'),(4,'s')")
    try:
        yield run
    finally:
        storage.close()


class TestNestedOrdering:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # Unwrapped — this always worked, and is the baseline the wrapped
            # forms have to match.
            ("SELECT array_agg(i ORDER BY i DESC)::text FROM a2", "{4,3,2,1}"),
            ("SELECT array_agg(i ORDER BY i)::text FROM a2", "{1,2,3,4}"),
            ("SELECT array_to_string(array_agg(s ORDER BY i DESC), ',') FROM a2", "w,z,y,x"),
            ("SELECT array_to_string(array_agg(s ORDER BY s), '-') FROM a2", "w-x-y-z"),
            (
                "SELECT upper(array_to_string(array_agg(s ORDER BY i DESC), '')) FROM a2",
                "WZYX",
            ),
            # Multi-key, and a direction on the leading key only.
            ("SELECT array_agg(i ORDER BY g, i DESC)::text FROM a2", "{2,1,4,3}"),
            ("SELECT array_agg(i ORDER BY g DESC, i)::text FROM a2", "{3,4,1,2}"),
        ],
    )
    def test_wrapped_keeps_its_order(self, db, sql, want):
        assert db(sql) == [(want,)]

    def test_subscript(self, db):
        """Subscripting is the shape where a dropped ORDER BY changes a scalar,
        not just a rendering."""
        assert db("SELECT (array_agg(i ORDER BY i DESC))[1] FROM a2") == [(4,)]

    def test_grouped(self, db):
        rows = db("SELECT g, array_agg(i ORDER BY i DESC)::text FROM a2 GROUP BY g ORDER BY g")
        assert rows == [(1, "{2,1}"), (2, "{4,3}")]

    def test_alongside_another_aggregate(self, db):
        assert db("SELECT array_agg(i ORDER BY i DESC), count(*) FROM a2") == [([4, 3, 2, 1], 4)]


class TestJoinPath:
    """The join planners have their own registrar, with the same defect."""

    def test_join(self, db):
        assert db(
            "SELECT array_agg(b2.v ORDER BY a2.i DESC)::text FROM a2 JOIN b2 ON a2.i=b2.i"
        ) == [("{s,r,q,p}",)]

    def test_join_grouped(self, db):
        rows = db(
            "SELECT a2.g, array_agg(b2.v ORDER BY a2.i DESC)::text FROM a2 JOIN b2"
            " ON a2.i=b2.i GROUP BY a2.g ORDER BY a2.g"
        )
        assert rows == [(1, "{q,p}"), (2, "{s,r}")]


class TestUnorderedIsUnchanged:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT array_agg(i)::text FROM a2", "{1,2,3,4}"),
            ("SELECT array_to_string(array_agg(s), ',') FROM a2", "x,y,z,w"),
        ],
    )
    def test_no_order_by(self, db, sql, want):
        assert db(sql) == [(want,)]


class TestStillRefused:
    """GROUPING SETS rejects ANY computed projection — `count(*)::text` too, so
    this is not about the aggregate's ordering. An honest 0A000, recorded in the
    backlog rather than fixed here."""

    @pytest.mark.parametrize(
        "sql",
        [
            "SELECT g, array_agg(i ORDER BY i DESC)::text FROM a2 GROUP BY GROUPING SETS ((g),())",
            "SELECT g, count(*)::text FROM a2 GROUP BY GROUPING SETS ((g),())",
        ],
    )
    def test_grouping_sets_computed_projection(self, db, sql):
        with pytest.raises(SQLError) as exc:
            db(sql)
        assert exc.value.sqlstate == "0A000"

    def test_grouping_sets_plain_projection_still_works(self, db):
        rows = db("SELECT g, count(*) FROM a2 GROUP BY GROUPING SETS ((g),())")
        assert sorted(rows, key=lambda r: (r[0] is None, r[0])) == [(1, 2), (2, 2), (None, 4)]
