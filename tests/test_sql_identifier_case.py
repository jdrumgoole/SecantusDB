"""Unquoted identifiers fold to lower case; quoted ones keep their spelling.

Postgres lower-cases an unquoted identifier, so ``AS TABLE_NAME`` and a later
``r.table_name`` name the same column, while ``"TABLE_NAME"`` is a different
one. Every spelling was compared exactly instead, so the two forms were two
different names. Code that wrote and read one spelling never tripped it;
anything generated, or written in SQL-standard upper case — JDBC's
``DatabaseMetaData`` queries being the case that found this — did.
"""

from __future__ import annotations

import pytest

from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def q(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE src (id int primary key, val int)")
    run("INSERT INTO src VALUES (1, 10)")
    try:
        yield run
    finally:
        storage.close()


class TestUnquotedFolds:
    def test_alias_written_upper_read_lower(self, q):
        assert q("SELECT r.table_name FROM (SELECT id AS TABLE_NAME FROM src) r") == [(1,)]

    def test_alias_written_lower_read_upper(self, q):
        assert q("SELECT r.TABLE_NAME FROM (SELECT id AS table_name FROM src) r") == [(1,)]

    def test_table_name(self, q):
        assert q("SELECT id FROM SRC") == [(1,)]
        assert q("SELECT id FROM SrC") == [(1,)]

    def test_column_reference(self, q):
        assert q("SELECT ID, VAL FROM src") == [(1, 10)]

    def test_where_and_order_by(self, q):
        q("INSERT INTO src VALUES (2, 5)")
        assert q("SELECT ID FROM SRC WHERE VAL < 9 ORDER BY ID") == [(2,)]

    def test_created_upper_read_lower(self, q):
        q("CREATE TABLE MyTable (Col int primary key)")
        q("INSERT INTO mytable VALUES (7)")
        assert q("SELECT col FROM MYTABLE") == [(7,)]

    def test_alias_in_group_by_and_having(self, q):
        q("INSERT INTO src VALUES (2, 10)")
        assert q("SELECT VAL, count(*) FROM SRC GROUP BY VAL HAVING count(*) > 1") == [(10, 2)]


class TestQuotedIsPreserved:
    def test_quoted_alias_keeps_case(self, q):
        assert q('SELECT r."Mixed" FROM (SELECT id AS "Mixed" FROM src) r') == [(1,)]

    def test_quoted_and_unquoted_are_different_names(self, q):
        """``"Mixed"`` is not ``mixed`` — this is the whole point of quoting."""
        with pytest.raises(Exception, match="does not exist"):
            q('SELECT r.mixed FROM (SELECT id AS "Mixed" FROM src) r')

    def test_quoted_table_name_keeps_case(self, q):
        q('CREATE TABLE "KeepCase" (a int primary key)')
        q('INSERT INTO "KeepCase" VALUES (3)')
        assert q('SELECT a FROM "KeepCase"') == [(3,)]
        with pytest.raises(Exception, match="does not exist"):
            q("SELECT a FROM keepcase")


class TestNotIdentifiers:
    def test_string_literals_keep_their_case(self, q):
        assert q("SELECT 'KeepMyCase'") == [("KeepMyCase",)]

    def test_string_comparison_is_still_case_sensitive(self, q):
        q("CREATE TABLE s (id int primary key, name text)")
        q("INSERT INTO s VALUES (1, 'Alice')")
        assert q("SELECT id FROM s WHERE name = 'Alice'") == [(1,)]
        assert q("SELECT id FROM s WHERE name = 'alice'") == []

    def test_public_is_a_keyword_not_a_folded_role(self, q):
        """``information_schema.role_table_grants`` reports the implicit role as
        ``PUBLIC`` however it was spelled — it is a keyword, not an identifier."""
        q("CREATE TABLE g (id int primary key)")
        q("GRANT SELECT ON g TO PUBLIC")
        rows = q("SELECT grantee FROM information_schema.role_table_grants WHERE table_name = 'g'")
        assert ("PUBLIC",) in rows
