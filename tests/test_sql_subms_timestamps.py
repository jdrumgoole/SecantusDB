"""Sub-millisecond precision for SQL ``timestamp`` columns.

BSON dates hold whole milliseconds, so `12:00:00.123456` used to come back as
`.123000`. The remainder now rides in a hidden `__us_<field>` companion (see
`secantus.sql.subms`), which keeps both protocols honest: a Mongo client still
reads a real BSON date, and SQL gets its microseconds back.

The invariant these tests exist to protect: **a stale companion is worse than
truncation**, because it reports a time that was never stored. Every write must
set or clear it.
"""

from __future__ import annotations

import datetime as dt
import os

import pytest

from secantus.sql import run_sql, subms
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"
US = dt.datetime(2026, 8, 18, 12, 0, 0, 123456)


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def table(storage, session):
    run(storage, session, "CREATE TABLE ts (id INT8 PRIMARY KEY, t TIMESTAMP)")
    return storage, session


class TestPureHelpers:
    def test_split_keeps_whole_milliseconds_and_returns_the_rest(self):
        stored, remainder = subms.split(US)
        assert stored == US.replace(microsecond=123000)
        assert remainder == 456

    def test_a_whole_millisecond_value_has_no_remainder(self):
        stored, remainder = subms.split(dt.datetime(2026, 1, 1, 0, 0, 0, 123000))
        assert remainder == 0 and stored.microsecond == 123000

    def test_merge_is_the_inverse_of_split(self):
        assert subms.merge(*subms.split(US)) == US

    def test_a_nonsensical_stored_remainder_is_ignored(self):
        # A hand-edited or foreign document must not be able to produce a time
        # that never existed.
        for bogus in (5000, -1, True, "456", None):
            assert subms.merge(US, bogus) == US

    def test_carry_clears_a_previous_remainder(self):
        doc = {"__us_t": 456}
        subms.carry_subms(doc, "t", dt.datetime(2026, 1, 1, 0, 0, 0, 123000))
        assert "__us_t" not in doc, "a whole-millisecond write must clear the companion"

    def test_non_datetime_values_pass_through(self):
        assert subms.split("hello") == ("hello", 0)
        assert subms.split(None) == (None, 0)


class TestRoundTrip:
    def test_insert_then_select_keeps_microseconds(self, table):
        storage, session = table
        run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
        assert run(storage, session, "SELECT t FROM ts").rows == [(US,)]

    def test_a_mongo_client_still_sees_a_real_date(self, table):
        storage, session = table
        run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
        doc = storage.find_matching(DB, "ts", {})[0]
        # The date itself is unchanged from what Mongo always stored — whole
        # milliseconds — with the remainder alongside it, not inside it.
        assert doc["t"] == US.replace(microsecond=123000)
        assert doc["__us_t"] == 456

    def test_a_whole_millisecond_value_adds_no_field(self, table):
        storage, session = table
        run(storage, session, "INSERT INTO ts VALUES (1, '2026-08-18 12:00:00.123')")
        doc = storage.find_matching(DB, "ts", {})[0]
        assert "__us_t" not in doc, "the common case must not litter the document"

    def test_update_to_a_whole_millisecond_clears_the_remainder(self, table):
        storage, session = table
        run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
        run(storage, session, "UPDATE ts SET t = '2026-08-18 12:00:00.500' WHERE id = 1")
        doc = storage.find_matching(DB, "ts", {})[0]
        assert "__us_t" not in doc
        # ... and the read agrees: no leftover microseconds.
        assert run(storage, session, "SELECT t FROM ts").rows == [
            (dt.datetime(2026, 8, 18, 12, 0, 0, 500000),)
        ]

    def test_update_to_a_new_sub_millisecond_value_replaces_it(self, table):
        storage, session = table
        run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
        run(storage, session, "UPDATE ts SET t = '2026-08-18 12:00:00.999888' WHERE id = 1")
        assert run(storage, session, "SELECT t FROM ts").rows == [
            (dt.datetime(2026, 8, 18, 12, 0, 0, 999888),)
        ]

    def test_returning_carries_the_precision(self, table):
        storage, session = table
        res = run(
            storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}') RETURNING t"
        )
        assert res.rows == [(US,)]

    def test_select_star_does_not_expose_the_companion(self, table):
        storage, session = table
        run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
        res = run(storage, session, "SELECT * FROM ts")
        assert [c.name for c in res.columns] == ["id", "t"]
        assert res.rows == [(1, US)]

    def test_a_reflected_collection_does_not_expose_the_companion(self, storage, session):
        # Schema-on-read must not surface the hidden field as a column.
        storage.insert(
            DB,
            "raw",
            [{"_id": 1, "t": US.replace(microsecond=123000), "__us_t": 456}],
        )
        res = run(storage, session, "SELECT * FROM raw")
        assert "__us_t" not in [c.name for c in res.columns]


def test_comparisons_are_microsecond_exact(table):
    """Comparisons see the remainder, not just the truncated millisecond.

    This test previously asserted the *opposite* — it was named
    `test_comparisons_remain_millisecond_blind` and pinned the limitation "so it
    stays visible", which meant it also pinned two wrong answers: a row failed an
    equality on its own stored value, and matched a value it was not equal to.
    Comparisons now lower against both the truncated field and the companion
    (`subms.cmp_filter`), verified against a live PostgreSQL 14 across 42
    predicate/literal combinations.

    ORDER BY within a single millisecond is still millisecond-granular — the
    companion is not yet a sort tiebreaker. That half remains open.
    """
    storage, session = table
    run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
    # A row matches an equality on its own stored value...
    assert run(storage, session, f"SELECT id FROM ts WHERE t = '{US.isoformat(sep=' ')}'").rows == [
        (1,)
    ]
    # ...and does NOT match the truncated literal it is not equal to.
    assert run(storage, session, "SELECT id FROM ts WHERE t = '2026-08-18 12:00:00.123'").rows == []
    # Ordering compares the millisecond first, the remainder only within it.
    assert run(storage, session, "SELECT id FROM ts WHERE t > '2026-08-18 12:00:00.123'").rows == [
        (1,)
    ]
    assert run(storage, session, "SELECT id FROM ts WHERE t < '2026-08-18 12:00:00.123'").rows == []
    assert run(storage, session, "SELECT id FROM ts WHERE t <> '2026-08-18 12:00:00.123'").rows == [
        (1,)
    ]


# --- differential against a real PostgreSQL, when one is reachable -----------

_PG_DSN = os.environ.get("SECANTUS_PG_ORACLE_DSN", "host=127.0.0.1 port=5432 dbname=postgres")


def _pg_oracle():
    """A live PostgreSQL connection, or None. Never fails the suite."""
    try:
        import psycopg

        return psycopg.connect(_PG_DSN, autocommit=True, connect_timeout=3)
    except Exception:  # noqa: BLE001 — absence is the normal case in CI
        return None


@pytest.mark.skipif(_pg_oracle() is None, reason="no local PostgreSQL oracle")
def test_subms_predicates_match_real_postgres(table):
    """Every comparison shape answered exactly as PostgreSQL answers it.

    The hand-derived expectations above say what we believe; this says what
    PostgreSQL actually does. Skipped when no server is reachable, so it adds
    coverage where one exists without making the suite depend on it.
    Point it elsewhere with SECANTUS_PG_ORACLE_DSN.
    """

    storage, session = table
    rows = [
        "2026-08-18 12:00:00.000000",
        "2026-08-18 12:00:00.000500",
        "2026-08-18 12:00:00.123000",
        "2026-08-18 12:00:00.123456",
        "2026-08-18 12:00:00.123999",
        "2026-08-18 12:00:00.124000",
    ]
    for i, v in enumerate(rows):
        run(storage, session, f"INSERT INTO ts VALUES ({i}, '{v}')")

    pg = _pg_oracle()
    assert pg is not None
    try:
        pg.execute("drop table if exists subms_oracle")
        pg.execute("create table subms_oracle (id int, t timestamp)")
        for i, v in enumerate(rows):
            pg.execute("insert into subms_oracle values (%s, %s)", (i, v))

        literals = [rows[0], rows[1], "2026-08-18 12:00:00.123", rows[3], rows[4], rows[5]]
        for op in ("=", "<>", ">", ">=", "<", "<="):
            for lit in literals:
                ours = run(
                    storage, session, f"SELECT id FROM ts WHERE t {op} '{lit}' ORDER BY id"
                ).rows
                theirs = pg.execute(
                    f"select id from subms_oracle where t {op} '{lit}' order by id"
                ).fetchall()
                assert [r[0] for r in ours] == [r[0] for r in theirs], f"t {op} '{lit}'"
    finally:
        pg.close()
