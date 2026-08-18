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


def test_comparisons_remain_millisecond_blind(table):
    """A KNOWN limitation, pinned so it stays visible.

    The stored date is still truncated, so a WHERE/ORDER BY on a timestamp
    column is answered at millisecond granularity — a sub-millisecond literal
    matches nothing, exactly as it did before this representation existed.
    Closing it means lowering comparisons against BOTH fields (and adding the
    companion as a sort tiebreaker); until then the read path is precise and
    the predicate path is not.
    """
    storage, session = table
    run(storage, session, f"INSERT INTO ts VALUES (1, '{US.isoformat(sep=' ')}')")
    assert (
        run(storage, session, f"SELECT id FROM ts WHERE t = '{US.isoformat(sep=' ')}'").rows == []
    )
    # The truncated literal does match — which is what the pushdown compares.
    assert run(storage, session, "SELECT id FROM ts WHERE t = '2026-08-18 12:00:00.123'").rows == [
        (1,)
    ]
