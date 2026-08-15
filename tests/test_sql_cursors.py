"""Server-side cursors: DECLARE … CURSOR / FETCH / MOVE / CLOSE.

The query is materialized at DECLARE; FETCH / MOVE walk a scroll position over
the stored rows (forward / backward / absolute / relative), so a cursor is fully
scrollable. WITHOUT HOLD cursors close at COMMIT / ROLLBACK; WITH HOLD survive.
Driven through ``run_sql`` over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    sess = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=sess)
    for i in range(1, 6):
        run_sql(s, DB, f"INSERT INTO t (id, n) VALUES ({i}, {i * 10})", session=sess)
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def fetch_ids(storage, session, sql):
    return [r[0] for r in q(storage, session, sql).rows]


def test_declare_and_fetch_forward(storage, session):
    assert q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id").command_tag == (
        "DECLARE CURSOR"
    )
    assert fetch_ids(storage, session, "FETCH 2 FROM c") == [1, 2]
    assert fetch_ids(storage, session, "FETCH NEXT FROM c") == [3]
    assert fetch_ids(storage, session, "FETCH FORWARD 1 c") == [4]
    assert fetch_ids(storage, session, "FETCH ALL FROM c") == [5]
    assert fetch_ids(storage, session, "FETCH ALL FROM c") == []  # exhausted


def test_fetch_count_tag_and_columns(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id, n FROM t ORDER BY id")
    res = q(storage, session, "FETCH 3 FROM c")
    assert res.command_tag == "FETCH 3"
    assert [col.name for col in res.columns] == ["id", "n"]
    assert res.rows == [(1, 10), (2, 20), (3, 30)]


def test_fetch_backward_and_prior(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH 4 FROM c") == [1, 2, 3, 4]
    assert fetch_ids(storage, session, "FETCH BACKWARD 2 FROM c") == [3, 2]
    assert fetch_ids(storage, session, "FETCH PRIOR FROM c") == [1]
    assert fetch_ids(storage, session, "FETCH PRIOR FROM c") == []  # before first


def test_fetch_absolute_relative_first_last(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH ABSOLUTE 3 FROM c") == [3]
    assert fetch_ids(storage, session, "FETCH RELATIVE 2 FROM c") == [5]
    assert fetch_ids(storage, session, "FETCH FIRST FROM c") == [1]
    assert fetch_ids(storage, session, "FETCH LAST FROM c") == [5]
    assert fetch_ids(storage, session, "FETCH ABSOLUTE -2 FROM c") == [4]


def test_move_positions_without_returning_rows(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    res = q(storage, session, "MOVE 2 FROM c")
    assert res.command_tag == "MOVE 2"
    assert res.rows == []  # MOVE returns no result set
    assert fetch_ids(storage, session, "FETCH NEXT FROM c") == [3]
    q(storage, session, "MOVE BACKWARD 2 c")
    assert fetch_ids(storage, session, "FETCH NEXT FROM c") == [2]


def test_cursor_over_join_query(storage, session):
    q(storage, session, "CREATE TABLE u (id bigint primary key, label text)")
    for i in (1, 2, 3):
        q(storage, session, f"INSERT INTO u (id, label) VALUES ({i}, 'x{i}')")
    q(
        storage,
        session,
        "DECLARE c CURSOR FOR SELECT t.id, u.label FROM t JOIN u ON t.id = u.id ORDER BY t.id",
    )
    assert q(storage, session, "FETCH ALL FROM c").rows == [(1, "x1"), (2, "x2"), (3, "x3")]


def test_close_cursor(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t")
    assert q(storage, session, "CLOSE c").command_tag == "CLOSE CURSOR"
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH 1 FROM c")
    assert ei.value.sqlstate == "34000"


def test_close_all(storage, session):
    q(storage, session, "DECLARE a CURSOR FOR SELECT id FROM t")
    q(storage, session, "DECLARE b CURSOR FOR SELECT id FROM t")
    q(storage, session, "CLOSE ALL")
    for name in ("a", "b"):
        with pytest.raises(SQLError):
            q(storage, session, f"FETCH 1 FROM {name}")


def test_without_hold_cursor_closes_at_commit(storage, session):
    q(storage, session, "BEGIN")
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH 1 FROM c") == [1]
    q(storage, session, "COMMIT")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH 1 FROM c")
    assert ei.value.sqlstate == "34000"


def test_with_hold_cursor_survives_commit(storage, session):
    q(storage, session, "BEGIN")
    q(storage, session, "DECLARE h CURSOR WITH HOLD FOR SELECT id FROM t ORDER BY id")
    q(storage, session, "COMMIT")
    assert fetch_ids(storage, session, "FETCH 2 FROM h") == [1, 2]


def test_fetch_unknown_cursor_errors(storage, session):
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH 1 FROM nope")
    assert ei.value.sqlstate == "34000"


def test_negative_bare_count_scans_backward(storage, session):
    # ``FETCH -n`` / ``MOVE -n`` move backward in the default direction (PG).
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    assert fetch_ids(storage, session, "FETCH 4 FROM c") == [1, 2, 3, 4]
    assert fetch_ids(storage, session, "FETCH -2 FROM c") == [3, 2]
    assert fetch_ids(storage, session, "FETCH FORWARD -1 c") == [1]


def test_move_absolute_zero_repositions_before_first(storage, session):
    q(storage, session, "DECLARE c CURSOR FOR SELECT id FROM t ORDER BY id")
    fetch_ids(storage, session, "FETCH ALL FROM c")
    q(storage, session, "MOVE ABSOLUTE 0 FROM c")
    assert fetch_ids(storage, session, "FETCH ALL FROM c") == [1, 2, 3, 4, 5]


def test_no_scroll_cursor_rejects_backward(storage, session):
    q(storage, session, "DECLARE c NO SCROLL CURSOR FOR SELECT id FROM t ORDER BY id")
    fetch_ids(storage, session, "FETCH 3 FROM c")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "FETCH BACKWARD 1 FROM c")
    assert ei.value.sqlstate == "55000"


def test_scroll_cursor_allows_backward(storage, session):
    q(storage, session, "DECLARE c SCROLL CURSOR FOR SELECT id FROM t ORDER BY id")
    fetch_ids(storage, session, "FETCH 3 FROM c")
    assert fetch_ids(storage, session, "FETCH BACKWARD 2 FROM c") == [2, 1]


def test_declare_non_query_body_is_syntax_error(storage, session):
    for body in ("wat", "CREATE TABLE ssc ()"):
        with pytest.raises(SQLError) as ei:
            q(storage, session, f"DECLARE c CURSOR FOR {body}")
        assert ei.value.sqlstate == "42601"


# --------------------------------------------------------------------------- #
# Resource caps (issue #194)
# --------------------------------------------------------------------------- #


def test_parse_rejects_overlong_statement():
    from secantus.sql import planner

    with pytest.raises(SQLError) as ei:
        planner.parse("SELECT 1" + " " * (planner.MAX_SQL_LENGTH + 1))
    assert ei.value.sqlstate == "54000"


def test_parse_rejects_deeply_nested_statement():
    from secantus.sql import planner

    # Hundreds of nested parens blow Python's recursion limit inside sqlglot;
    # we convert that to a clean 54000 instead of an uncaught RecursionError.
    with pytest.raises(SQLError) as ei:
        planner.parse("SELECT " + "(" * 500 + "1" + ")" * 500)
    assert ei.value.sqlstate == "54000"


def test_cursor_count_is_capped(storage, session, monkeypatch):
    from secantus.sql import engine

    monkeypatch.setattr(engine, "MAX_CURSORS_PER_SESSION", 2)
    q(storage, session, "DECLARE c1 CURSOR FOR SELECT id FROM t")
    q(storage, session, "DECLARE c2 CURSOR FOR SELECT id FROM t")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "DECLARE c3 CURSOR FOR SELECT id FROM t")
    assert ei.value.sqlstate == "54000"
    # Re-declaring an existing name replaces it (no net growth) — still allowed.
    q(storage, session, "DECLARE c1 CURSOR FOR SELECT id FROM t")


def test_cursor_row_count_is_capped(storage, session, monkeypatch):
    from secantus.sql import engine

    monkeypatch.setattr(engine, "MAX_CURSOR_ROWS", 3)
    # ``t`` has 5 rows; retaining all of them exceeds the cap.
    with pytest.raises(SQLError) as ei:
        q(storage, session, "DECLARE big CURSOR FOR SELECT id FROM t")
    assert ei.value.sqlstate == "54000"


def test_parse_accepts_megabyte_scale_statement():
    # The old 1 MB cap was falsified by a REAL query shape: pgx's
    # 65535-parameter statements are ~1.04 MB (real PG accepts up to its
    # 1 GB message limit). A statement over the old cap must parse.
    from secantus.sql import planner

    literal = "x" * 1_100_000
    stmts = planner.parse(f"SELECT length('{literal}')")
    assert len(stmts) == 1


def test_bare_expressions_are_syntax_errors(storage, session):
    # sqlglot parses bare words as column/aliased expressions; a
    # non-statement must be PG's 42601, not silently accepted (pgx
    # Prepare("SYNTAX ERROR")). The expression-shaped COMMANDS sqlglot
    # mis-parses the same way (CLOSE / DISCARD / DEALLOCATE) stay working.
    from secantus.sql import engine

    for sql in ("bad", "SYNTAX ERROR", "asdf"):
        with pytest.raises(SQLError) as ei:
            engine.run_sql(storage, session.database, sql, session=session)
        assert ei.value.sqlstate == "42601"
    assert engine.run_sql(storage, session.database, "DISCARD ALL", session=session)
