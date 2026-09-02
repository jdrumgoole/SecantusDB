"""Integer columns rejected nothing: out-of-range values were STORED.

Found 2026-09-01 by a type-boundary sweep against PostgreSQL 14.13. This is a
data-integrity defect, not a message nit:

    INSERT INTO t (i) VALUES (2147483648)   pg 22003    us INSERT 0 1
    SELECT i FROM t                                     us 2147483648

An `int` column held a value no `int` can hold, so the column's declared type
and its contents disagreed — and the RowDescription advertised oid 23 (four
bytes) for it, which is the "declared a type the values do not honour" shape
that the unnest element-type work already had to fix once elsewhere. `smallint`
and `bigint` were the same.

The check lives in `typemap.check_int_range`, called from the shared integer
coercion, so every write path inherits it. The CAST path is guarded at
`_eval_cast`'s single exit rather than in each branch — that body has a dozen
returns (enum, bit, char-length, range, array …) and adding a guard to each is
how one gets missed.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

_INT4_MAX, _INT4_MIN = 2**31 - 1, -(2**31)
_INT2_MAX = 2**15 - 1
_INT8_MAX = 2**63 - 1


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE ov (i int, sm smallint, b bigint)")
    try:
        yield run
    finally:
        storage.close()


def _err(db, sql):
    with pytest.raises(SQLError) as ei:
        db(sql)
    return ei.value.sqlstate, str(ei.value)


class TestStorageRejectsOutOfRange:
    """The integrity half — these used to succeed and store the value."""

    @pytest.mark.parametrize(
        ("sql", "msg"),
        [
            (f"INSERT INTO ov (i) VALUES ({_INT4_MAX + 1})", "integer out of range"),
            (f"INSERT INTO ov (i) VALUES ({_INT4_MIN - 1})", "integer out of range"),
            (f"INSERT INTO ov (sm) VALUES ({_INT2_MAX + 1})", "smallint out of range"),
            (f"INSERT INTO ov (b) VALUES ({_INT8_MAX + 1})", "bigint out of range"),
            ("INSERT INTO ov (i) VALUES (1e10)", "integer out of range"),
        ],
    )
    def test_insert_is_22003(self, db, sql, msg):
        assert _err(db, sql) == ("22003", msg)

    def test_an_overflowing_expression_cannot_reach_a_column(self, db):
        db(f"INSERT INTO ov (i) VALUES ({_INT4_MAX})")
        assert _err(db, f"INSERT INTO ov (i) VALUES ({_INT4_MAX} + 1)")[0] == "22003"
        assert _err(db, "UPDATE ov SET i = i + 1")[0] == "22003"
        assert db("SELECT i FROM ov") == [(_INT4_MAX,)]

    def test_the_boundaries_themselves_are_accepted(self, db):
        db(f"INSERT INTO ov (i, sm, b) VALUES ({_INT4_MAX}, {_INT2_MAX}, {_INT8_MAX})")
        db(f"INSERT INTO ov (i) VALUES ({_INT4_MIN})")
        assert db("SELECT count(*) FROM ov") == [(2,)]


class TestCastRejectsOutOfRange:
    @pytest.mark.parametrize(
        ("expr", "msg"),
        [
            ("1e10::int", "integer out of range"),
            (f"{_INT4_MAX + 1}::int", "integer out of range"),
            (f"{_INT2_MAX + 1}::smallint", "smallint out of range"),
            (f"{_INT8_MAX + 1}::bigint", "bigint out of range"),
            # `-2147483648::int` binds the cast to the POSITIVE literal, which
            # overflows — PostgreSQL rejects this spelling as well.
            (f"{_INT4_MIN}::int", "integer out of range"),
        ],
    )
    def test_cast_is_22003(self, db, expr, msg):
        assert _err(db, f"SELECT {expr}") == ("22003", msg)

    @pytest.mark.parametrize(
        "expr",
        [
            f"{_INT4_MAX}::int",
            # PARENTHESISED: `-2147483648::int` binds as `-(2147483648::int)`,
            # and the positive literal IS out of range — PostgreSQL raises
            # `integer out of range` for that spelling too (verified), so the
            # parens are the difference, not a leniency of ours.
            f"({_INT4_MIN})::int",
            f"{_INT2_MAX}::smallint",
            "1.9::int",
            "'42'::int",
            "(-1.5)::int",
        ],
    )
    def test_in_range_casts_are_unaffected(self, db, expr):
        db(f"SELECT {expr}")  # must not raise
