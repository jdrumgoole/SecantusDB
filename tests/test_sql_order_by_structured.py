"""`ORDER BY` over a `jsonb` or range column was an internal error.

Both ride as bare Python subdocuments, so the sort's `x < y` raised
`TypeError: '<' not supported between instances of 'dict' and 'dict'` and the
client saw `XX000`.

PostgreSQL's jsonb order was **measured** against 14.13 rather than taken from
the manual, which matters because a TOP-LEVEL empty array sorts before
everything, `null` included. That is not a documented rule but a consequence of
storage: a top-level scalar is held as a one-element array, so `[]` is simply
the shorter container. Nested, `[]` is an ordinary array.

The key is decided from the COLUMN, not from the value. Keying only the values
that fail to compare gives an order that is not even transitive — Python
compares `False < 1` quite happily, so `false` landed between two numbers.

`'null'::jsonb` and SQL NULL are both Python `None` here, so a JSON null sorts
where SQL NULL does rather than inside the jsonb order. Recorded, not fixed.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

#: Every shape, in the order PostgreSQL 14.13 puts them in.
JSONB_IN_PG_ORDER = [
    "[]",  # a top-level empty array beats everything
    '""',
    '"a"',
    '"b"',
    "-3",
    "1",
    "1.5",
    "2",
    "false",
    "true",
    '["a"]',
    "[1]",
    "[2]",
    "[true]",
    "[[]]",
    "[[1]]",
    "[{}]",
    "[1,2]",
    "[1,1,1]",
    "{}",
    '{"a":1}',
    '{"a":2}',
    '{"b":1}',
    '{"bb":1}',
    '{"c":1}',
    '{"a":1,"b":2}',
    '{"c":1,"aa":2}',
]

#: Ranges, in PostgreSQL's order: empty first, then by lower bound (unbounded
#: lowest), then by upper (unbounded highest).
RANGES_IN_PG_ORDER = ["empty", "(,3)", "[0,3)", "[1,5)", "[1,9)", "[1,)", "[2,4)"]


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE jb (i int, v jsonb)")
    run("CREATE TABLE rg (i int, v int4range)")
    # Inserted in a scrambled order so a passing test cannot be insertion order.
    for i, literal in sorted(enumerate(JSONB_IN_PG_ORDER), key=lambda p: p[1]):
        run(f"INSERT INTO jb VALUES ({i}, '{literal}')")
    for i, literal in sorted(enumerate(RANGES_IN_PG_ORDER), key=lambda p: p[1]):
        run(f"INSERT INTO rg VALUES ({i}, '{literal}')")
    try:
        yield run
    finally:
        storage.close()


def _ids(rows):
    return [r[0] for r in rows]


class TestJsonbOrder:
    def test_ascending(self, db):
        assert _ids(db("SELECT i FROM jb ORDER BY v, i")) == list(range(len(JSONB_IN_PG_ORDER)))

    def test_descending(self, db):
        assert _ids(db("SELECT i FROM jb ORDER BY v DESC, i DESC")) == list(
            reversed(range(len(JSONB_IN_PG_ORDER)))
        )

    def test_null_placement_still_works(self, db):
        db("INSERT INTO jb VALUES (99, NULL)")
        assert _ids(db("SELECT i FROM jb ORDER BY v, i"))[-1] == 99
        assert _ids(db("SELECT i FROM jb ORDER BY v NULLS FIRST, i"))[0] == 99

    @pytest.mark.parametrize(
        ("lower", "higher"),
        [
            # The three rules that are easy to get backwards.
            ("[]", "null"),  # a top-level empty array beats null
            ("[1]", "[1,2]"),  # arrays compare LENGTH first
            ('{"a":1}', '{"a":1,"b":2}'),  # objects compare PAIR COUNT first
            ('{"bb":1}', '{"c":1}'),  # keys compare plainly, not by length
            ("2", "false"),  # every number is below every boolean
            ("true", "[1]"),  # every boolean is below a non-empty array
        ],
    )
    def test_pairwise(self, db, lower, higher):
        db("DELETE FROM jb")
        db(f"INSERT INTO jb VALUES (1, '{higher}'), (2, '{lower}')")
        assert _ids(db("SELECT i FROM jb ORDER BY v")) == [2, 1]


class TestRangeOrder:
    def test_ascending(self, db):
        assert _ids(db("SELECT i FROM rg ORDER BY v, i")) == list(range(len(RANGES_IN_PG_ORDER)))

    def test_descending(self, db):
        assert _ids(db("SELECT i FROM rg ORDER BY v DESC, i DESC")) == list(
            reversed(range(len(RANGES_IN_PG_ORDER)))
        )


class TestOtherSortPaths:
    """A partition of homogeneous containers orders correctly in the window and
    `array_agg` paths too, and neither is an internal error any more."""

    @pytest.fixture()
    def objs(self, db):
        db("DELETE FROM jb")
        for i, literal in enumerate(["{}", '{"a":1}', '{"a":2}', '{"b":1}', '{"a":1,"b":2}']):
            db(f"INSERT INTO jb VALUES ({i}, '{literal}')")
        return db

    def test_window(self, objs):
        rows = objs("SELECT i, row_number() OVER (ORDER BY v, i) FROM jb ORDER BY i")
        assert dict(rows) == {0: 1, 1: 2, 2: 3, 3: 4, 4: 5}

    def test_array_agg(self, objs):
        assert objs("SELECT array_agg(i ORDER BY v, i) FROM jb") == [([0, 1, 2, 3, 4],)]

    def test_plain_order_by(self, objs):
        assert _ids(objs("SELECT i FROM jb ORDER BY v, i")) == [0, 1, 2, 3, 4]
