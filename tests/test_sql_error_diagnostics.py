"""PG error-diagnostic identity fields on constraint violations (pgjdbc's
ServerErrorTest): every violation carries the ErrorResponse fields a real PG
attaches — s(chema), t(able), c(olumn), n(constraint), d(atatype) — which
drivers surface via ``ServerErrorMessage`` / ``psycopg``'s ``diag``. Also the
equality-only ``EXCLUDE (col WITH =)`` constraint (23P01 exclusion_violation).
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def st(tmp_path):
    s = Storage(str(tmp_path))
    try:
        sess = Session(database=DB)
        for ddl in (
            "CREATE DOMAIN testdom AS int4 CHECK (value < 10)",
            "CREATE TABLE testerr (id int not null, val testdom not null)",
            "ALTER TABLE testerr ADD CONSTRAINT testerr_pk PRIMARY KEY (id)",
            "INSERT INTO testerr (id, val) VALUES (1, 1)",
        ):
            run_sql(s, DB, ddl, session=sess)
        yield s
    finally:
        s.close()


def _violation(st, sql):
    with pytest.raises(errors.SQLError) as exc_info:
        run_sql(st, DB, sql, session=Session(database=DB))
    return exc_info.value


class TestViolationDiagnostics:
    def test_primary_key(self, st):
        e = _violation(st, "INSERT INTO testerr (id, val) VALUES (1, 1)")
        assert e.sqlstate == "23505"
        assert e.diag == {"s": "public", "t": "testerr", "n": "testerr_pk"}

    def test_not_null_column(self, st):
        e = _violation(st, "INSERT INTO testerr (id, val) VALUES (2, NULL)")
        assert e.sqlstate == "23502"
        assert e.diag == {"s": "public", "t": "testerr", "c": "val"}

    def test_not_null_omitted_column(self, st):
        e = _violation(st, "INSERT INTO testerr (val) VALUES (1)")
        assert e.sqlstate == "23502"
        assert e.diag == {"s": "public", "t": "testerr", "c": "id"}

    def test_domain_check(self, st):
        e = _violation(st, "INSERT INTO testerr (id, val) VALUES (3, 20)")
        assert e.sqlstate == "23514"
        assert e.diag == {"s": "public", "d": "testdom", "n": "testdom_check"}

    def test_foreign_key(self, st):
        sess = Session(database=DB)
        run_sql(
            st,
            DB,
            "CREATE TABLE testerr_foreign (id int not null, testerr_id int, "
            "CONSTRAINT testerr FOREIGN KEY (testerr_id) references testerr(id))",
            session=sess,
        )
        e = _violation(st, "INSERT INTO testerr_foreign (id, testerr_id) VALUES (1, 2)")
        assert e.sqlstate == "23503"
        assert e.diag["s"] == "public"
        assert e.diag["t"] == "testerr_foreign"

    def test_table_check(self, st):
        sess = Session(database=DB)
        run_sql(
            st,
            DB,
            "CREATE TABLE testerr_check (id int not null, max10 int CHECK (max10 < 11))",
            session=sess,
        )
        e = _violation(st, "INSERT INTO testerr_check (id, max10) VALUES (2, 11)")
        assert e.sqlstate == "23514"
        assert e.diag["s"] == "public"
        assert e.diag["t"] == "testerr_check"

    def test_declared_unique_constraint(self, st):
        sess = Session(database=DB)
        run_sql(
            st,
            DB,
            "CREATE TABLE uqt (id int PRIMARY KEY, v int, CONSTRAINT uq_v UNIQUE (v))",
            session=sess,
        )
        run_sql(st, DB, "INSERT INTO uqt VALUES (1, 5)", session=sess)
        e = _violation(st, "INSERT INTO uqt VALUES (2, 5)")
        assert e.sqlstate == "23505"
        assert e.diag["n"] == "uq_v"
        assert e.diag["t"] == "uqt"


class TestExclusionConstraint:
    def test_equality_exclude_violation_is_23p01(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE ex (id int, EXCLUDE (id WITH =))", session=sess)
        run_sql(st, DB, "INSERT INTO ex (id) VALUES (1108)", session=sess)
        e = _violation(st, "INSERT INTO ex (id) VALUES (1108)")
        assert e.sqlstate == "23P01"
        assert "exclusion constraint" in e.message
        assert e.diag == {"s": "public", "t": "ex", "n": "ex_id_excl"}

    def test_distinct_values_coexist(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE TABLE ex2 (id int, EXCLUDE (id WITH =))", session=sess)
        run_sql(st, DB, "INSERT INTO ex2 (id) VALUES (1)", session=sess)
        run_sql(st, DB, "INSERT INTO ex2 (id) VALUES (2)", session=sess)
        rows = run_sql(st, DB, "SELECT count(*) FROM ex2", session=sess)[-1].rows
        assert rows == [(2,)]

    def test_non_equality_operator_rejected(self, st):
        with pytest.raises(errors.SQLError) as exc_info:
            run_sql(
                st,
                DB,
                "CREATE TABLE exr (r int, EXCLUDE (r WITH <>))",
                session=Session(database=DB),
            )
        assert exc_info.value.sqlstate == "0A000"
