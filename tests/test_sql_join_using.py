"""``JOIN … USING (col)`` joins on the named columns.

Nothing in join planning read sqlglot's ``args["using"]``, so a USING join
lost its condition and degraded to a CROSS JOIN — a silent wrong answer on
ordinary SQL. Every expectation below was checked against a real PostgreSQL
14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    for ddl in (
        "CREATE TABLE a(k int, v text)",
        "INSERT INTO a VALUES (1, 'x'), (2, 'y')",
        "CREATE TABLE b(k int, w text)",
        "INSERT INTO b VALUES (1, 'p'), (3, 'q')",
        "CREATE TABLE c(k int, z text)",
        "INSERT INTO c VALUES (1, 'm'), (2, 'n')",
    ):
        run(ddl)
    try:
        yield run
    finally:
        storage.close()


class TestUsingJoinsRatherThanCrossJoining:
    def test_inner_join(self, db):
        assert db("SELECT v, w FROM a JOIN b USING (k)") == [("x", "p")]

    def test_left_join_keeps_unmatched_left_rows(self, db):
        assert db("SELECT v, w FROM a LEFT JOIN b USING (k) ORDER BY v") == [
            ("x", "p"),
            ("y", None),
        ]

    def test_chained_using_joins(self, db):
        assert db("SELECT v, w, z FROM a JOIN b USING (k) JOIN c USING (k)") == [("x", "p", "m")]

    def test_derived_table_on_the_right(self, db):
        assert db("SELECT v, w FROM a JOIN (SELECT k, w FROM b) AS d USING (k)") == [("x", "p")]

    def test_multi_column_using(self, db):
        db("CREATE TABLE m1(k int, j int, v text)")
        db("INSERT INTO m1 VALUES (1, 1, 'hit'), (1, 2, 'miss')")
        db("CREATE TABLE m2(k int, j int, w text)")
        db("INSERT INTO m2 VALUES (1, 1, 'ok')")
        assert db("SELECT v, w FROM m1 JOIN m2 USING (k, j)") == [("hit", "ok")]

    def test_using_with_a_where_clause(self, db):
        assert db("SELECT v FROM a JOIN b USING (k) WHERE w = 'p'") == [("x",)]

    def test_aggregate_over_a_using_join(self, db):
        assert db("SELECT count(*) FROM a JOIN b USING (k)") == [(1,)]


class TestUnaffectedJoinForms:
    def test_explicit_on_is_unchanged(self, db):
        assert db("SELECT a.v, b.w FROM a JOIN b ON a.k = b.k") == [("x", "p")]

    def test_cross_join_still_crosses(self, db):
        assert db("SELECT count(*) FROM a CROSS JOIN b") == [(4,)]

    def test_comma_join_still_crosses(self, db):
        assert db("SELECT count(*) FROM a, b") == [(4,)]


class TestStarMerge:
    def test_star_merges_the_joined_column(self, db):
        """Postgres merges the USING column: ``SELECT *`` returns ``k`` once
        (from the left side), then each source's remaining columns. Fixed by
        ``planner.expand_using_star`` — a single pre-desugar AST rewrite —
        after being pinned as a known divergence."""
        assert db("SELECT * FROM a JOIN b USING (k)") == [(1, "x", "p")]

    def test_tbl_star_does_not_merge(self, db):
        """``a.*`` is NOT merged by Postgres — only the bare ``*`` is."""
        assert db("SELECT a.* FROM a JOIN b USING (k)") == [(1, "x")]

    def test_on_join_star_unchanged(self, db):
        assert db("SELECT * FROM a JOIN b ON a.k = b.k") == [(1, "x", 1, "p")]


def test_double_paren_join_chain_with_table_wrapped_values(db):
    """CrystalReports' ``{oj (((...)))}`` shape: extra grouping parens nest
    join-less Subquery wrappers AND make sqlglot parse the aliased VALUES as
    a Table wrapping the Values node — both peeled by the planner now."""
    rows = db(
        "select t1.id, t2.id, t3.id"
        " from (((values (1, 'one'), (2, 'two')) as t1 (id, text)"
        " left outer join (values (1, 'a'), (3, 'b')) as t2 (id, text) on (t1.id = t2.id))"
        " left outer join (values (1, '1'), (4, '2')) as t3 (id, text) on (t2.id = t3.id))"
    )
    assert rows == [(1, 1, 1), (2, None, None)]
