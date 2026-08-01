"""Unqualified relation names resolve through ``search_path``.

Before this, an unqualified name only ever meant the ``public`` key, so a
``SET search_path TO other_schema`` followed by a bare reference raised
``relation "…" does not exist`` — the largest failure cluster in the pgjdbc
gauge (``UpdateableResultTest``, which deliberately puts an *empty* schema
first in the path to prove resolution walks past it).
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

    run.session = session  # type: ignore[attr-defined]
    try:
        yield run
    finally:
        storage.close()


class TestSearchPathProperty:
    def test_defaults_to_public(self):
        assert Session(database="t").search_path == ["public"]

    def test_user_collapses_to_public_and_dedups(self, db):
        db('SET search_path TO "$user", public, s1')
        assert db.session.search_path == ["public", "s1"]

    def test_current_schema_is_the_first_entry(self, db):
        db("SET search_path TO s1, public")
        assert db.session.current_schema == "s1"


class TestResolution:
    def test_bare_name_resolves_through_the_path(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (1), (2)")
        db("SET search_path TO s1, public")
        assert db("SELECT a FROM t ORDER BY a") == [(1,), (2,)]

    def test_walks_past_an_earlier_schema_without_the_table(self, db):
        """UpdateableResultTest's shape: the first path entry is a real schema
        that simply does not hold the relation."""
        db("CREATE SCHEMA empty_one")
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (7)")
        db("SET search_path TO empty_one, s1, public")
        assert db("SELECT a FROM t") == [(7,)]

    def test_earlier_entry_wins_when_both_hold_the_name(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE SCHEMA s2")
        db("CREATE TABLE s1.t(a int)")
        db("CREATE TABLE s2.t(a int)")
        db("INSERT INTO s1.t VALUES (1)")
        db("INSERT INTO s2.t VALUES (2)")
        db("SET search_path TO s2, s1, public")
        assert db("SELECT a FROM t") == [(2,)]

    def test_public_still_shadows_a_path_schema(self, db):
        """A name that already resolves is never redirected — the pass only
        consults the path when the bare key misses."""
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (1)")
        db("CREATE TABLE t(a int)")
        db("INSERT INTO t VALUES (99)")
        db("SET search_path TO s1, public")
        assert db("SELECT a FROM t") == [(99,)]

    def test_unknown_name_still_errors(self, db):
        db("CREATE SCHEMA s1")
        db("SET search_path TO s1, public")
        with pytest.raises(Exception, match="does not exist"):
            db("SELECT * FROM nosuch")


class TestWritesLandInTheResolvedSchema:
    """Read and write must agree: the storage key is composed from the same
    node the resolver matched, so an unqualified INSERT cannot land somewhere
    the matching SELECT will not look."""

    @pytest.fixture()
    def seeded(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (1), (2)")
        db("SET search_path TO s1, public")
        return db

    def test_insert(self, seeded):
        seeded("INSERT INTO t VALUES (3)")
        assert seeded("SELECT a FROM s1.t ORDER BY a") == [(1,), (2,), (3,)]

    def test_update(self, seeded):
        seeded("UPDATE t SET a = 50 WHERE a = 1")
        assert seeded("SELECT a FROM s1.t ORDER BY a") == [(2,), (50,)]

    def test_delete(self, seeded):
        seeded("DELETE FROM t WHERE a = 1")
        assert seeded("SELECT a FROM s1.t") == [(2,)]

    def test_insert_select_over_the_same_bare_name(self, seeded):
        seeded("INSERT INTO t SELECT a + 10 FROM t")
        assert seeded("SELECT a FROM s1.t ORDER BY a") == [(1,), (2,), (11,), (12,)]

    def test_drop(self, seeded):
        seeded("DROP TABLE t")
        with pytest.raises(Exception, match="does not exist"):
            seeded("SELECT * FROM s1.t")


class TestCreateTargetIsExempt:
    def test_create_does_not_bind_to_an_existing_relation_on_the_path(self, db):
        """Postgres creates into the path's first schema; it never resolves a
        create target onto a same-named relation further along. Without this
        exemption the CREATE silently aliased s1.t and the INSERT below landed
        in it."""
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (1), (2)")
        db("SET search_path TO s1, public")
        db("CREATE TABLE t(a int)")
        db("INSERT INTO t VALUES (777)")
        assert db("SELECT a FROM t") == [(777,)]
        assert db("SELECT a FROM s1.t ORDER BY a") == [(1,), (2,)]

    def test_create_index_target_does_resolve(self, db):
        """CREATE INDEX names an *existing* relation, so it is not exempt."""
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("SET search_path TO s1, public")
        db("CREATE INDEX ix ON t(a)")
        db("INSERT INTO t VALUES (4)")
        assert db("SELECT a FROM s1.t") == [(4,)]


class TestCteNamesAreNotRewritten:
    def test_cte_shadows_a_path_relation(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (1), (2)")
        db("SET search_path TO s1, public")
        assert db("WITH t AS (SELECT 5 AS a) SELECT a FROM t") == [(5,)]

    def test_cte_over_a_path_relation_still_reads_it(self, db):
        db("CREATE SCHEMA s1")
        db("CREATE TABLE s1.t(a int)")
        db("INSERT INTO s1.t VALUES (1), (2)")
        db("SET search_path TO s1, public")
        assert db("WITH c AS (SELECT a FROM t) SELECT count(*) FROM c") == [(2,)]


class TestNestedAggregatesAreNotFolded:
    """A FROM-less SELECT feeds its own aggregates one implicit row, but a
    nested SELECT has its own row source. Folding used to walk into the
    subquery, so ``SELECT (SELECT count(*) FROM t)`` answered 1 for any table
    — a silent wrong answer — and the other aggregates raised instead."""

    @pytest.fixture()
    def seeded(self, db):
        db("CREATE TABLE t(a int)")
        db("INSERT INTO t VALUES (5), (6)")
        return db

    def test_count_counts_the_subquery_rows(self, seeded):
        assert seeded("SELECT (SELECT count(*) FROM t) AS m") == [(2,)]

    @pytest.mark.parametrize(
        "sql,expected",
        [
            ("SELECT (SELECT max(a) FROM t) AS m", 6),
            ("SELECT (SELECT min(a) FROM t) AS m", 5),
            ("SELECT (SELECT sum(a) FROM t) AS m", 11),
            ("SELECT (SELECT avg(a) FROM t) AS m", 5.5),
        ],
    )
    def test_other_aggregates_resolve_the_subquery_column(self, seeded, sql, expected):
        assert seeded(sql) == [(expected,)]

    def test_two_subqueries_in_one_projection(self, seeded):
        assert seeded("SELECT (SELECT min(a) FROM t), (SELECT max(a) FROM t)") == [(5, 6)]

    def test_arithmetic_over_a_subquery_aggregate(self, seeded):
        assert seeded("SELECT (SELECT count(*) FROM t) + 10 AS m") == [(12,)]

    def test_outer_and_inner_fold_independently(self, seeded):
        """The outer count(*) still sees the implicit single row (1) while the
        inner one sees the table (2)."""
        assert seeded("SELECT count(*), (SELECT count(*) FROM t)") == [(1, 2)]

    def test_outer_folding_is_unchanged(self, db):
        assert db("SELECT count(*)") == [(1,)]
        assert db("SELECT max(3)") == [(3,)]
        assert db("SELECT count(*) WHERE 1 = 2") == [(0,)]
        assert db("SELECT max(3) WHERE 1 = 2") == [(None,)]
