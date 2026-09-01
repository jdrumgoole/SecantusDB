"""``IN`` requires a parenthesised list or a subquery.

sqlglot accepts a BARE right-hand side (``x IN $1``) and parks it under the
node's ``field`` arg. We then compared against it and quietly matched nothing,
so `WHERE id IN %s` — the common psycopg slip, whose working spelling is
`= ANY(%s)` — returned ZERO ROWS where PostgreSQL 14.13 answers
`42601 syntax error at or near "$1"`.

Silent emptiness is the worst available answer: it looks like data rather than
a bug, and the query that produced it is one a user is likely to write.

Found 2026-09-01 by a probe that itself contained the mistake — the sweep
flagged the difference before the probe's own bug was noticed.
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

    run("CREATE TABLE inx (id int, s text)")
    run("INSERT INTO inx VALUES (1, 'a'), (2, 'b'), (3, 'c')")
    try:
        yield run
    finally:
        storage.close()


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT id FROM inx WHERE id IN $1",
        "SELECT id FROM inx WHERE id NOT IN $1",
        "SELECT id FROM inx WHERE s IN $1",
        "UPDATE inx SET s = 'z' WHERE id IN $1",
        "DELETE FROM inx WHERE id IN $1",
    ],
)
def test_bare_right_hand_side_is_a_syntax_error(db, sql):
    with pytest.raises(SQLError) as ei:
        db(sql)
    assert ei.value.sqlstate == "42601"
    assert "syntax error" in str(ei.value)


@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        ("SELECT id FROM inx WHERE id IN (1, 3) ORDER BY id", [(1,), (3,)]),
        ("SELECT id FROM inx WHERE id IN (1) ORDER BY id", [(1,)]),
        ("SELECT id FROM inx WHERE id NOT IN (1) ORDER BY id", [(2,), (3,)]),
        (
            "SELECT id FROM inx WHERE id IN (SELECT id FROM inx WHERE id < 3) ORDER BY id",
            [(1,), (2,)],
        ),
        ("SELECT id FROM inx WHERE id = ANY(ARRAY[1, 3]) ORDER BY id", [(1,), (3,)]),
        ("SELECT id FROM inx WHERE s IN ('a', 'c') ORDER BY id", [(1,), (3,)]),
    ],
)
def test_the_valid_shapes_are_untouched(db, sql, expected):
    """The guard keys on the node's ``field`` arg, which only the bare form
    sets — a list carries ``expressions`` and a subquery carries ``query``."""
    assert db(sql) == expected
