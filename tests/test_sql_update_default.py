"""``UPDATE … SET col = DEFAULT``.

It answered `42703 column "default" does not exist`: in an UPDATE, sqlglot
parses the `DEFAULT` keyword as an unquoted **Column** named `default`, not as
the `Var` a VALUES tuple gets. `_is_default_cell` only recognised the `Var`
form, so the assignment fell through to the per-row-expression path and
`default` was resolved as a column name.

The same mis-detection sat under the GENERATED-column guard, which uses the
same helper to decide whether an update is legal — so that check was reading
every `SET gen_col = DEFAULT` as a non-DEFAULT value.

A QUOTED `"default"` is a real column reference and is deliberately left alone.
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

    run(
        "CREATE TABLE sd (id int, n int DEFAULT 7, s text, "
        "t text DEFAULT 'x', nn int NOT NULL DEFAULT 3)"
    )
    run("INSERT INTO sd VALUES (1, 1, 'a', 'b', 1)")
    try:
        yield run
    finally:
        storage.close()


def test_literal_default(db):
    assert db("UPDATE sd SET n = DEFAULT RETURNING n") == [(7,)]


def test_a_column_with_no_default_becomes_null(db):
    assert db("UPDATE sd SET s = DEFAULT RETURNING s") == [(None,)]


def test_two_defaults_in_one_statement(db):
    assert db("UPDATE sd SET n = DEFAULT, t = DEFAULT RETURNING n, t") == [(7, "x")]


def test_not_null_column_with_a_default(db):
    assert db("UPDATE sd SET nn = DEFAULT RETURNING nn") == [(3,)]


def test_ordinary_assignment_is_unaffected(db):
    assert db("UPDATE sd SET n = 5 RETURNING n") == [(5,)]


def test_an_expression_assignment_is_unaffected(db):
    assert db("UPDATE sd SET n = n + 1 RETURNING n") == [(2,)]


def test_a_quoted_default_is_a_column_reference(db):
    """`"default"` in quotes names a column, and there is none — so this must
    stay an undefined-column error rather than becoming the DEFAULT keyword."""
    with pytest.raises(SQLError) as ei:
        db('UPDATE sd SET n = "default"')
    assert ei.value.sqlstate == "42703"


def test_not_null_without_a_default_is_a_violation(db):
    db("CREATE TABLE nd (id int, r int NOT NULL)")
    db("INSERT INTO nd VALUES (1, 1)")
    with pytest.raises(SQLError) as ei:
        db("UPDATE nd SET r = DEFAULT")
    assert ei.value.sqlstate == "23502"


def test_a_sequence_backed_column_is_refused_not_guessed(db):
    """A serial's DEFAULT is `nextval(...)`, which the planner cannot draw —
    refused explicitly rather than written as something else."""
    db("CREATE TABLE sq (id serial, v int)")
    db("INSERT INTO sq (v) VALUES (1)")
    with pytest.raises(SQLError) as ei:
        db("UPDATE sq SET id = DEFAULT")
    assert ei.value.sqlstate == "0A000"
