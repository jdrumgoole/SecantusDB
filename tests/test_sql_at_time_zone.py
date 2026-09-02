"""`AT TIME ZONE`, `LIKE ALL/ANY`, and the `timezone_*` extract fields.

Three features the sixth sweep found refused, plus an error whose explanation
had been folded into the message.

`AT TIME ZONE` reads BOTH ways, and which way depends on the operand: a NAIVE
timestamp is interpreted as being in the zone and becomes an instant, while an
AWARE one is converted into the zone and loses the zone.

`extract(timezone_hour …)` reports the SESSION zone's offset, not the literal's
— PostgreSQL normalises a timestamptz into the session zone before extracting,
so `'…+05'::timestamptz` gives 0 under a UTC session, not 5.

Every expectation here was measured against PostgreSQL 14.13.
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
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows[0][0] if res.rows else None

    return run


class TestAtTimeZone:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # A naive timestamp is IN the zone, and becomes an instant.
            (
                "SELECT ('2020-06-15 12:00:00'::timestamp AT TIME ZONE 'America/New_York')::text",
                "2020-06-15 16:00:00+00",
            ),
            (
                "SELECT ('2020-06-15 12:00:00'::timestamp AT TIME ZONE 'UTC')::text",
                "2020-06-15 12:00:00+00",
            ),
            # An instant is converted INTO the zone, and loses it.
            (
                "SELECT ('2020-06-15 12:00:00+00'::timestamptz "
                "AT TIME ZONE 'America/New_York')::text",
                "2020-06-15 08:00:00",
            ),
            # Winter, so the offset differs from the summer case above.
            (
                "SELECT ('2020-01-15 12:00:00'::timestamp AT TIME ZONE 'America/New_York')::text",
                "2020-01-15 17:00:00+00",
            ),
        ],
    )
    def test_both_directions(self, db, sql, want):
        assert db(sql) == want

    def test_null_operand(self, db):
        assert db("SELECT NULL::timestamp AT TIME ZONE 'UTC'") is None

    def test_unknown_zone(self, db):
        with pytest.raises(SQLError) as exc:
            db("SELECT '2020-01-01'::timestamp AT TIME ZONE 'Nowhere/Bad'")
        assert exc.value.sqlstate == "22023"


class TestLikeQuantified:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT 'abc' LIKE ALL(ARRAY['a%','%c'])", True),
            ("SELECT 'abc' LIKE ALL(ARRAY['a%','x%'])", False),
            ("SELECT 'abc' LIKE ANY(ARRAY['x%','a%'])", True),
            ("SELECT 'abc' LIKE ANY(ARRAY['x%','y%'])", False),
            ("SELECT 'abc' NOT LIKE ALL(ARRAY['x%','y%'])", True),
            ("SELECT 'ABC' ILIKE ANY(ARRAY['a%'])", True),
            # The scalar form was always supported.
            ("SELECT 'abc' LIKE 'a%'", True),
        ],
    )
    def test_quantified(self, db, sql, want):
        assert db(sql) is want

    def test_null_propagates(self, db):
        assert db("SELECT NULL LIKE ANY(ARRAY['a%'])") is None


class TestTimezoneExtract:
    @pytest.mark.parametrize(
        ("field", "want"),
        [("timezone", 0), ("timezone_hour", 0), ("timezone_minute", 0)],
    )
    def test_session_offset_not_the_literals(self, db, field, want):
        """PG normalises the value into the session zone first, so a literal's
        own `+05` does not survive to be reported."""
        assert db(f"SELECT extract({field} from '2020-01-01 00:00:00+05'::timestamptz)") == want

    def test_other_fields_unaffected(self, db):
        assert db("SELECT extract(hour from '2020-01-01 07:00:00'::timestamp)") == 7


class TestIdentityErrorDetail:
    """PostgreSQL puts the explanation in DETAIL; folding it into the message
    made a message no client can match on."""

    def test_identity_always(self, db):
        db("CREATE TABLE g1 (id int GENERATED ALWAYS AS IDENTITY, n int)")
        with pytest.raises(SQLError) as exc:
            db("INSERT INTO g1 (id, n) VALUES (99, 1)")
        assert exc.value.sqlstate == "428C9"
        assert exc.value.message == 'cannot insert a non-DEFAULT value into column "id"'
        assert exc.value.diag["D"] == (
            'Column "id" is an identity column defined as GENERATED ALWAYS.'
        )

    def test_generated_column(self, db):
        db("CREATE TABLE g2 (n int, dbl int GENERATED ALWAYS AS (n*2) STORED)")
        with pytest.raises(SQLError) as exc:
            db("INSERT INTO g2 (n, dbl) VALUES (1, 1)")
        assert exc.value.message == 'cannot insert a non-DEFAULT value into column "dbl"'
        assert exc.value.diag["D"] == 'Column "dbl" is a generated column.'
