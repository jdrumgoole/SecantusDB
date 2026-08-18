"""HAVING shapes the `$match` lowerer can't express, and casts to text.

Postgres evaluates HAVING over the grouped rows, so any boolean expression is
legal there. SecantusDB lowers what it can into a `$match` and used to raise
`0A000` for everything else — `HAVING NOT (count(*) > 1)`, `HAVING count(*) * 2
> 3`, a CASE, a function call. Those now fall back to the same per-grouped-row
residual route the HAVING-subquery case already used.

Fixing that surfaced a second, worse bug: a cast to text did not produce text,
so `count(*)::text = '2'` compared the number 2 to the string '2' and returned
NO rows — a wrong answer rather than an error. Rendering had hidden it, since
2 and '2' go on the wire as the same bytes.

Every expected value here was probed against a real PostgreSQL 14.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql import run_sql
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
def grouped(storage, session):
    """Two groups: 'a' has two rows (n=1,3), 'b' has one (n=5)."""
    run(storage, session, "CREATE TABLE t (g TEXT, n INT8)")
    run(storage, session, "INSERT INTO t VALUES ('a',1),('a',3),('b',5)")
    return storage, session


# Expected rows are PostgreSQL 14's, for the same table.
@pytest.mark.parametrize(
    ("having", "expected"),
    [
        ("count(*) > 1", [("a", 2)]),
        ("count(*) BETWEEN 2 AND 5", [("a", 2)]),
        ("NOT (count(*) > 1)", [("b", 1)]),
        ("sum(n) + count(*) > 4", [("a", 2), ("b", 1)]),
        ("count(*) * 2 > 3", [("a", 2)]),
        ("abs(sum(n)) > 2", [("a", 2), ("b", 1)]),
        ("count(*)::text = '2'", [("a", 2)]),
        ("CASE WHEN count(*) > 1 THEN true ELSE false END", [("a", 2)]),
        ("max(n) - min(n) > 0", [("a", 2)]),
        ("coalesce(sum(n), 0) > 2", [("a", 2), ("b", 1)]),
    ],
)
def test_having_shapes_match_postgres(grouped, having, expected):
    storage, session = grouped
    sql = f"SELECT g, count(*) AS c FROM t GROUP BY g HAVING {having} ORDER BY g"
    assert run(storage, session, sql).rows == expected


def test_a_real_having_error_still_surfaces(grouped):
    # The residual fallback must not swallow genuine user errors: a bare column
    # that is neither grouped nor aggregated is 42803, not a deferred residual
    # that would then evaluate it as an ordinary expression.
    storage, session = grouped
    from secantus.sql.errors import SQLError

    with pytest.raises(SQLError) as exc:
        run(storage, session, "SELECT g FROM t GROUP BY g HAVING n > 1")
    assert exc.value.sqlstate == "42803"


class TestCastToText:
    """A cast to text must PRODUCE text — the spellings are Postgres 14's."""

    @pytest.mark.parametrize(
        ("literal", "expected"),
        [
            ("2", "2"),
            ("2.0::float8", "2"),  # trailing .0 is dropped
            ("2.5::float8", "2.5"),
            ("true", "true"),  # NOT the wire form 't'
            ("false", "false"),
            ("2.50::numeric", "2.50"),  # numeric keeps its scale
        ],
    )
    def test_scalar_casts_render_like_postgres(self, storage, session, literal, expected):
        assert run(storage, session, f"SELECT ({literal})::text AS r").rows == [(expected,)]

    def test_a_text_cast_compares_as_text(self, storage, session):
        # The bug: this compared 2 to '2' and was false.
        assert run(storage, session, "SELECT (2::text = '2') AS r").rows == [(True,)]
        assert run(storage, session, "SELECT (2::text = '2.0') AS r").rows == [(False,)]

    def test_evaluator_converts_each_numeric_kind(self):
        import sqlglot

        from secantus.sql.scalar import evaluate

        node = sqlglot.parse_one("SELECT c::text AS r", read="postgres").expressions[0].this
        cases = [
            (2, "2"),
            (2.0, "2"),
            (2.5, "2.5"),
            (True, "true"),
            (False, "false"),
            (Decimal("2.50"), "2.50"),
            (1e20, "1e+20"),
            ("already text", "already text"),
        ]
        for value, expected in cases:
            got = evaluate(node, lambda _col, _v=value: _v, None)
            assert got == expected, f"{value!r} cast to text should be {expected!r}, got {got!r}"
