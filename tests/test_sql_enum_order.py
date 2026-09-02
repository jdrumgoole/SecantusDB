"""Finishing the enum story: comparison in a SELECT list, and `enum_range`.

The WHERE half was fixed by rewriting a range comparison into the set of
labels that satisfy it. A comparison in the SELECT *list* has to yield a
BOOLEAN instead and is evaluated by `scalar`, which has no catalog — so
`SELECT m > 'ok'` still answered by SPELLING while `WHERE m > 'ok'` did not,
which is a worse state than either being wrong on its own. The planner now
stamps the label list on the comparison node for the evaluator to read.

`enum_range` / `enum_first` / `enum_last` take their enum type from the
ARGUMENT'S CAST — the argument is a NULL — so they cannot go through the
value-only builtin table and were `0A000`.

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

    run("CREATE TYPE mood AS ENUM ('sad','ok','happy')")
    run("CREATE TABLE e1 (id int, m mood)")
    run("INSERT INTO e1 VALUES (1,'happy'),(2,'sad'),(3,'ok')")
    try:
        yield run
    finally:
        storage.close()


class TestComparisonInProjection:
    @pytest.mark.parametrize(
        ("expr", "want"),
        [
            ("m > 'ok'::mood", [True, False, False]),
            ("m < 'ok'", [False, True, False]),
            ("m >= 'ok'", [True, False, True]),
            ("m <= 'ok'", [False, True, True]),
            ("'ok' < m", [True, False, False]),
            # Equality compares by label and was always right.
            ("m = 'ok'", [False, False, True]),
        ],
    )
    def test_projection(self, db, expr, want):
        rows = db(f"SELECT id, {expr} FROM e1 ORDER BY id")
        assert [r[1] for r in rows] == want

    def test_where_still_right(self, db):
        assert [r[0] for r in db("SELECT id FROM e1 WHERE m > 'ok' ORDER BY id")] == [1]

    def test_order_by_still_right(self, db):
        assert db("SELECT id, m FROM e1 ORDER BY m") == [(2, "sad"), (3, "ok"), (1, "happy")]

    def test_a_text_column_is_unaffected(self, db):
        db("CREATE TABLE t1 (id int, s text)")
        db("INSERT INTO t1 VALUES (1,'happy'),(2,'sad')")
        rows = db("SELECT id, s > 'ok' FROM t1 ORDER BY id")
        # Plain text still compares by spelling: 'happy' < 'ok' < 'sad'.
        assert [r[1] for r in rows] == [False, True]


class TestEnumFunctions:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT enum_range(NULL::mood)", ["sad", "ok", "happy"]),
            ("SELECT enum_first(NULL::mood)", "sad"),
            ("SELECT enum_last(NULL::mood)", "happy"),
        ],
    )
    def test_functions(self, db, sql, want):
        assert db(sql)[0][0] == want

    def test_unknown_type(self, db):
        with pytest.raises(SQLError) as exc:
            db("SELECT enum_range(NULL::nope)")
        assert exc.value.sqlstate == "42704"
