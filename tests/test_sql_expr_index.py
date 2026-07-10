"""Expression indexes (``CREATE INDEX … ((expr))``).

Postgres lets an index cover a computed expression — ``CREATE INDEX ON t
((lower(name)))`` — so a query whose WHERE clause contains that exact expression
plans through the index. SecantusDB materialises the expression into a hidden
per-row field (``__expr_<index>``, recomputed on every write) and builds an
ordinary single-field B-tree over it; a matching ``WHERE`` is rewritten to
reference the hidden field so it rides the normal index path (``IXSCAN``). The
hidden field never appears in ``SELECT *`` or reflection, and dropping the index
unregisters the expression and strips the field from every row.

Driven end-to-end through ``run_sql`` over the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.engine import Catalog
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run(s, session, "CREATE TABLE t (id int PRIMARY KEY, name text, a int, b int)")
    run(
        s,
        session,
        "INSERT INTO t VALUES (1, 'Bob', 3, 7), (2, 'BOB', 4, 6), (3, 'zed', 1, 1)",
    )
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def _hidden_field(storage, index_name):
    ei = next(ei for ei in Catalog(storage).get(DB, "t").expr_indexes if ei.name == index_name)
    return ei.field


# -- creation + WHERE acceleration ------------------------------------------ #


def test_create_function_expression_index(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    # A matching WHERE returns the case-folded matches …
    assert rows(storage, session, "SELECT id FROM t WHERE lower(name) = 'bob'") == [(1,), (2,)]
    # … and rides the hidden-field B-tree (IXSCAN, not a scan).
    field = _hidden_field(storage, "t_lower")
    assert storage.explain_plan(DB, "t", {field: "bob"})["kind"] == "IXSCAN"


def test_create_arithmetic_expression_index(storage, session):
    run(storage, session, "CREATE INDEX t_ab ON t ((a + b))")
    assert rows(storage, session, "SELECT id FROM t WHERE a + b = 10") == [(1,), (2,)]
    field = _hidden_field(storage, "t_ab")
    assert storage.explain_plan(DB, "t", {field: 10})["kind"] == "IXSCAN"


def test_expression_index_used_in_explain(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    plan = "\n".join(
        r[0] for r in rows(storage, session, "EXPLAIN SELECT id FROM t WHERE lower(name) = 'bob'")
    )
    assert "Index Scan using t_lower" in plan


# -- write maintenance ------------------------------------------------------- #


def test_insert_maintains_expression_index(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    run(storage, session, "INSERT INTO t VALUES (4, 'BOB', 5, 5)")
    assert rows(storage, session, "SELECT id FROM t WHERE lower(name) = 'bob'") == [
        (1,),
        (2,),
        (4,),
    ]


def test_update_maintains_expression_index(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    run(storage, session, "UPDATE t SET name = 'bob' WHERE id = 3")
    assert rows(storage, session, "SELECT id FROM t WHERE lower(name) = 'bob'") == [
        (1,),
        (2,),
        (3,),
    ]


def test_backfill_covers_preexisting_rows(storage, session):
    # The index is created *after* the rows exist — the backfill must populate the
    # hidden field for every existing row.
    run(storage, session, "CREATE INDEX t_ab ON t ((a + b))")
    assert rows(storage, session, "SELECT id FROM t WHERE a + b = 2") == [(3,)]


# -- hygiene: the hidden field never leaks ----------------------------------- #


def test_select_star_hides_hidden_field(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    cols = [c.name for c in run(storage, session, "SELECT * FROM t").columns]
    assert cols == ["id", "name", "a", "b"]


def test_hidden_field_not_in_reflection(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    names = {
        r[0]
        for r in rows(
            storage,
            session,
            "SELECT column_name FROM information_schema.columns WHERE table_name = 't'",
        )
    }
    assert names == {"id", "name", "a", "b"}


# -- ORDER BY on the expression stays correct (per-row, not index-order) ----- #


def test_order_by_expression_still_correct(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    # bob/BOB/zed → sorted case-insensitively: Bob(1), BOB(2), zed(3).
    assert [r[0] for r in rows(storage, session, "SELECT id FROM t ORDER BY lower(name)")] == [
        1,
        2,
        3,
    ]


# -- persistence + DROP INDEX cleanup ---------------------------------------- #


def test_expression_index_persists_in_catalog(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    # A freshly-loaded catalog still carries the registration (so the WHERE rewrite
    # fires across connections, not only within the creating one).
    assert [ei.name for ei in Catalog(storage).get(DB, "t").expr_indexes] == ["t_lower"]


def test_drop_index_unregisters_and_strips_field(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    field = _hidden_field(storage, "t_lower")
    run(storage, session, "DROP INDEX t_lower")
    # Unregistered from the catalog …
    assert Catalog(storage).get(DB, "t").expr_indexes == []
    # … the hidden field is stripped from every row …
    assert not any(field in doc for doc in storage.find_matching(DB, "t", {}))
    # … and the query still returns correct results (now via a plain scan).
    assert rows(storage, session, "SELECT id FROM t WHERE lower(name) = 'bob'") == [(1,), (2,)]


def test_drop_one_of_two_expression_indexes(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    run(storage, session, "CREATE INDEX t_ab ON t ((a + b))")
    run(storage, session, "DROP INDEX t_lower")
    assert [ei.name for ei in Catalog(storage).get(DB, "t").expr_indexes] == ["t_ab"]
    # The surviving index still accelerates its expression.
    field = _hidden_field(storage, "t_ab")
    assert storage.explain_plan(DB, "t", {field: 10})["kind"] == "IXSCAN"


def test_expression_index_still_reflects_in_pg_index(storage, session):
    run(storage, session, "CREATE INDEX t_lower ON t ((lower(name)))")
    assert rows(
        storage,
        session,
        "SELECT relname FROM pg_catalog.pg_class WHERE relkind = 'i' AND relname = 't_lower'",
    ) == [("t_lower",)]


# -- rejections -------------------------------------------------------------- #


def _sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


def test_mixing_expression_and_column_rejected(storage, session):
    # An index that mixes a plain column with an expression isn't supported.
    assert _sqlstate(storage, session, "CREATE INDEX ix ON t (a, (b + 1))") == "0A000"


def test_unevaluable_expression_rejected(storage, session):
    # An expression the engine can't evaluate is a faithful not-supported.
    assert _sqlstate(storage, session, "CREATE INDEX ix ON t ((some_unknown_fn(a)))") == "0A000"
