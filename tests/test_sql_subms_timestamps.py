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

import pg_oracle
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


def _pg_oracle():
    """A live PostgreSQL to check against, or None. Point elsewhere with
    SECANTUS_PG_ORACLE_DSN.

    Delegates to `pg_oracle` so all six oracle suites share one probe, and one
    skip reason that says why. The inline copies this replaced had drifted to
    three different default DSNs and skipped with a message indistinguishable
    from "PostgreSQL is not installed".
    """
    return pg_oracle.connect()


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
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


# ---------------------------------------------------------------------------
# ORDER BY -- the other half of the sub-millisecond entry. Predicates were
# fixed first; the sort key stayed millisecond-granular, so rows inside one
# millisecond came back in storage order.
# ---------------------------------------------------------------------------

#: Deliberately out of order, and three of the four share a millisecond, so a
#: millisecond-granular key leaves them in insertion order and looks "sorted".
ORDER_ROWS = [
    (1, "2026-08-18 12:00:00.123900"),
    (2, "2026-08-18 12:00:00.123100"),
    (3, "2026-08-18 12:00:00.123500"),
    (4, "2026-08-18 12:00:00.122000"),
]


@pytest.fixture
def ordered_table(storage, session):
    run(storage, session, "CREATE TABLE ord (id int, t timestamp)")
    for i, v in ORDER_ROWS:
        run(storage, session, f"INSERT INTO ord VALUES ({i}, '{v}')")
    return storage, session


def test_order_by_is_microsecond_exact(ordered_table):
    storage, session = ordered_table
    got = [r[0] for r in run(storage, session, "SELECT id FROM ord ORDER BY t").rows]
    assert got == [4, 2, 3, 1]


def test_order_by_desc_is_microsecond_exact(ordered_table):
    storage, session = ordered_table
    got = [r[0] for r in run(storage, session, "SELECT id FROM ord ORDER BY t DESC").rows]
    assert got == [1, 3, 2, 4]


def test_order_by_ordinal_sorts_and_returns_microseconds(ordered_table):
    """The projected-expression path is a SEPARATE route from a plain column
    ORDER BY: it never went through the read-side companion merge, so this
    shape both sorted at millisecond granularity AND returned truncated times.
    Both halves are asserted here."""
    storage, session = ordered_table
    rows = run(storage, session, "SELECT id, t FROM ord ORDER BY 2").rows
    assert [r[0] for r in rows] == [4, 2, 3, 1]
    assert [r[1].microsecond for r in rows] == [122000, 123100, 123500, 123900]


def test_order_by_alias_is_microsecond_exact(ordered_table):
    storage, session = ordered_table
    rows = run(storage, session, "SELECT id, t AS when_ FROM ord ORDER BY when_").rows
    assert [r[0] for r in rows] == [4, 2, 3, 1]


def test_distinct_on_picks_the_microsecond_first_row(storage, session):
    """DISTINCT ON keeps the first row per key in the ORDER BY order, so a
    millisecond-granular sort silently picked the wrong row -- a value bug, not
    just an ordering one."""
    run(storage, session, "CREATE TABLE don (id int, g text, t timestamp)")
    for i, g, v in [
        (1, "a", "2026-08-18 12:00:00.123900"),
        (2, "a", "2026-08-18 12:00:00.123100"),
    ]:
        run(storage, session, f"INSERT INTO don VALUES ({i}, '{g}', '{v}')")
    got = run(storage, session, "SELECT DISTINCT ON (g) id FROM don ORDER BY g, t").rows
    assert [r[0] for r in got] == [2]


def test_order_by_limit_takes_the_microsecond_smallest(ordered_table):
    """LIMIT reads off the sorted list, so a wrong key returns wrong ROWS, not
    merely a wrong order."""
    storage, session = ordered_table
    got = [r[0] for r in run(storage, session, "SELECT id FROM ord ORDER BY t LIMIT 2").rows]
    assert got == [4, 2]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_subms_order_by_matches_real_postgres(ordered_table):
    """The same shapes against the live server rather than a hand-derived
    expectation. `min`/`max` and in-call `array_agg(... ORDER BY t)` are
    deliberately NOT here -- they are pushed into the Mongo pipeline, which
    never sees the companion, and are filed as their own item."""
    storage, session = ordered_table
    pg = _pg_oracle()
    assert pg is not None
    try:
        pg.execute("drop table if exists subms_ord_oracle")
        pg.execute("create table subms_ord_oracle (id int, t timestamp)")
        for i, v in ORDER_ROWS:
            pg.execute("insert into subms_ord_oracle values (%s, %s)", (i, v))
        for suffix in ("", " DESC", " ASC"):
            ours = run(storage, session, f"SELECT id FROM ord ORDER BY t{suffix}").rows
            theirs = pg.execute(
                f"select id from subms_ord_oracle order by t{suffix.lower()}"
            ).fetchall()
            assert [r[0] for r in ours] == [r[0] for r in theirs], f"ORDER BY t{suffix}"
    finally:
        pg.close()


# ---------------------------------------------------------------------------
# Aggregates. A BSON date cannot carry the remainder, so `min` / `max` and the
# ordered-aggregate sort keys accumulate a `{__subms_d, __subms_u}` composite
# inside the pipeline and the executor merges it back.
#
# `min(t)` returning a whole millisecond was the worst of this family: not a
# lost ordering but a TIMESTAMP THAT WAS NEVER STORED.
# ---------------------------------------------------------------------------


@pytest.fixture
def grouped_table(storage, session):
    run(storage, session, "CREATE TABLE agg (id int, g text, t timestamp)")
    for i, g, v in [
        (1, "a", "2026-08-18 12:00:00.123900"),
        (2, "a", "2026-08-18 12:00:00.123100"),
        (3, "b", "2026-08-18 12:00:00.123500"),
        (4, "b", "2026-08-18 12:00:00.122000"),
    ]:
        run(storage, session, f"INSERT INTO agg VALUES ({i}, '{g}', '{v}')")
    run(storage, session, "INSERT INTO agg VALUES (5, 'c', NULL)")
    return storage, session


def test_min_max_keep_microseconds(grouped_table):
    storage, session = grouped_table
    rows = run(storage, session, "SELECT g, min(t), max(t) FROM agg GROUP BY g ORDER BY g").rows
    assert [(r[0], r[1] and r[1].microsecond, r[2] and r[2].microsecond) for r in rows] == [
        ("a", 123100, 123900),
        ("b", 122000, 123500),
        ("c", None, None),
    ]


def test_min_of_an_all_null_group_is_null(grouped_table):
    """The composite must be NULL for a NULL row, not a document wrapping one --
    otherwise the accumulator stops skipping it and every group gains a value."""
    storage, session = grouped_table
    rows = run(storage, session, "SELECT min(t) FROM agg WHERE g = 'c'").rows
    assert rows == [(None,)]


def test_min_with_filter_keeps_microseconds(grouped_table):
    storage, session = grouped_table
    rows = run(
        storage,
        session,
        "SELECT g, min(t) FILTER (WHERE id <> 2) FROM agg GROUP BY g ORDER BY g",
    ).rows
    assert [(r[0], r[1] and r[1].microsecond) for r in rows] == [
        ("a", 123900),
        ("b", 122000),
        ("c", None),
    ]


def test_array_agg_orders_by_microseconds(grouped_table):
    storage, session = grouped_table
    rows = run(
        storage, session, "SELECT g, array_agg(id ORDER BY t) FROM agg GROUP BY g ORDER BY g"
    ).rows
    assert [(r[0], r[1]) for r in rows] == [("a", [2, 1]), ("b", [4, 3]), ("c", [5])]


def test_string_agg_orders_by_microseconds(grouped_table):
    storage, session = grouped_table
    rows = run(
        storage,
        session,
        "SELECT g, string_agg(id::text, ',' ORDER BY t) FROM agg GROUP BY g ORDER BY g",
    ).rows
    assert [(r[0], r[1]) for r in rows] == [("a", "2,1"), ("b", "4,3"), ("c", "5")]


@pytest.mark.parametrize(
    "predicate,expected",
    [
        ("min(t) > '2026-08-18 12:00:00.123000'", ["a"]),
        ("min(t) >= '2026-08-18 12:00:00.123100'", ["a"]),
        ("min(t) < '2026-08-18 12:00:00.123100'", ["b"]),
        ("min(t) = '2026-08-18 12:00:00.122000'", ["b"]),
        ("max(t) < '2026-08-18 12:00:00.123900'", ["b"]),
    ],
)
def test_having_on_min_max_is_microsecond_exact(grouped_table, predicate, expected):
    """HAVING compares against the accumulator's output, so the literal has to be
    lowered into the composite shape too. Three of these five were already wrong
    before the composite existed, and `= ` was right ONLY because its literal
    happened to land on a whole millisecond -- so this parametrisation is what
    keeps the composite from regressing the one case that used to work."""
    storage, session = grouped_table
    rows = run(storage, session, f"SELECT g FROM agg GROUP BY g HAVING {predicate} ORDER BY g").rows
    assert [r[0] for r in rows] == expected


def test_order_by_an_aggregate_still_works(grouped_table):
    """`ORDER BY min(t)` sorts on the accumulator output; it must see a merged
    datetime, not the raw composite."""
    storage, session = grouped_table
    rows = run(storage, session, "SELECT g FROM agg GROUP BY g ORDER BY min(t)").rows
    assert [r[0] for r in rows] == ["b", "a", "c"]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_subms_aggregates_match_real_postgres(grouped_table):
    storage, session = grouped_table
    pg = _pg_oracle()
    assert pg is not None
    try:
        pg.execute("drop table if exists subms_agg_oracle")
        pg.execute("create table subms_agg_oracle (id int, g text, t timestamp)")
        for i, g, v in [
            (1, "a", "2026-08-18 12:00:00.123900"),
            (2, "a", "2026-08-18 12:00:00.123100"),
            (3, "b", "2026-08-18 12:00:00.123500"),
            (4, "b", "2026-08-18 12:00:00.122000"),
            (5, "c", None),
        ]:
            pg.execute("insert into subms_agg_oracle values (%s, %s, %s)", (i, g, v))
        for sql in (
            "select g, min(t), max(t) from {} group by g order by g",
            "select g, array_agg(id order by t) from {} group by g order by g",
            "select g from {} group by g having min(t) > '2026-08-18 12:00:00.123000' order by g",
        ):
            ours = [tuple(r) for r in run(storage, session, sql.format("agg")).rows]
            theirs = [tuple(r) for r in pg.execute(sql.format("subms_agg_oracle")).fetchall()]
            assert ours == theirs, sql
    finally:
        pg.close()


# ---------------------------------------------------------------------------
# GROUP BY. The group KEY was the truncated date, so rows differing only in
# microseconds merged into one group -- which makes the aggregate values over
# those groups wrong, not merely the key.
# ---------------------------------------------------------------------------


@pytest.fixture
def keyed_table(storage, session):
    run(storage, session, "CREATE TABLE gk (id int, t timestamp)")
    for i, v in [
        (1, "2026-08-18 12:00:00.123100"),
        (2, "2026-08-18 12:00:00.123500"),
        (3, "2026-08-18 12:00:00.123100"),
        (4, "2026-08-18 12:00:00.122000"),
    ]:
        run(storage, session, f"INSERT INTO gk VALUES ({i}, '{v}')")
    return storage, session


def test_group_by_timestamp_does_not_merge_microseconds(keyed_table):
    """Three distinct times used to collapse into two groups -- so `count(*)`
    answered 3 where Postgres answers 2 and 1, and the emitted key was
    `.123000`, a time no row held."""
    storage, session = keyed_table
    rows = run(storage, session, "SELECT t, count(*) FROM gk GROUP BY t ORDER BY t").rows
    assert [(r[0].microsecond, r[1]) for r in rows] == [(122000, 1), (123100, 2), (123500, 1)]


def test_aggregates_over_merged_groups_were_wrong(keyed_table):
    """The value half of the same bug: a merged group summed rows that belong to
    different groups."""
    storage, session = keyed_table
    rows = run(storage, session, "SELECT t, sum(id) FROM gk GROUP BY t ORDER BY t").rows
    assert [(r[0].microsecond, r[1]) for r in rows] == [(122000, 4), (123100, 4), (123500, 2)]


@pytest.mark.parametrize(
    "predicate,expected",
    [
        ("t > '2026-08-18 12:00:00.123000'", [123100, 123500]),
        ("t = '2026-08-18 12:00:00.123100'", [123100]),
        ("t < '2026-08-18 12:00:00.123500'", [122000, 123100]),
    ],
)
def test_having_on_a_timestamp_group_key(keyed_table, predicate, expected):
    """A HAVING term over the GROUP BY key compares against the key, which is
    now the composite -- so the literal needs the same lowering the min/max case
    needed. All three shapes were wrong before."""
    storage, session = keyed_table
    rows = run(storage, session, f"SELECT t FROM gk GROUP BY t HAVING {predicate} ORDER BY t").rows
    assert [r[0].microsecond for r in rows] == expected


def test_order_by_a_timestamp_group_key(keyed_table):
    storage, session = keyed_table
    rows = run(storage, session, "SELECT t FROM gk GROUP BY t ORDER BY t DESC").rows
    assert [r[0].microsecond for r in rows] == [123500, 123100, 122000]


def test_group_by_with_a_where_clause(keyed_table):
    storage, session = keyed_table
    rows = run(
        storage, session, "SELECT t, count(*) FROM gk WHERE id < 4 GROUP BY t ORDER BY t"
    ).rows
    assert [(r[0].microsecond, r[1]) for r in rows] == [(123100, 2), (123500, 1)]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_subms_group_by_matches_real_postgres(keyed_table):
    storage, session = keyed_table
    pg = _pg_oracle()
    assert pg is not None
    try:
        pg.execute("drop table if exists subms_gk_oracle")
        pg.execute("create table subms_gk_oracle (id int, t timestamp)")
        for i, v in [
            (1, "2026-08-18 12:00:00.123100"),
            (2, "2026-08-18 12:00:00.123500"),
            (3, "2026-08-18 12:00:00.123100"),
            (4, "2026-08-18 12:00:00.122000"),
        ]:
            pg.execute("insert into subms_gk_oracle values (%s, %s)", (i, v))
        for sql in (
            "select t, count(*) from {} group by t order by t",
            "select t, sum(id) from {} group by t order by t",
            "select t from {} group by t having t > '2026-08-18 12:00:00.123000' order by t",
        ):
            ours = [tuple(r) for r in run(storage, session, sql.format("gk")).rows]
            theirs = [tuple(r) for r in pg.execute(sql.format("subms_gk_oracle")).fetchall()]
            assert ours == theirs, sql
    finally:
        pg.close()


# ---------------------------------------------------------------------------
# DISTINCT -- the fourth and last route in this family. It dedups on the
# PROJECTED value, and the projection dropped the microseconds, so it both
# merged rows and returned a time none of them held.
# ---------------------------------------------------------------------------


@pytest.fixture
def distinct_table(storage, session):
    run(storage, session, "CREATE TABLE dt (id int, g text, t timestamp)")
    for i, g, v in [
        (1, "x", "2026-08-18 12:00:00.123100"),
        (2, "x", "2026-08-18 12:00:00.123500"),
        (3, "y", "2026-08-18 12:00:00.123100"),
        (4, "y", "2026-08-18 12:00:00.122000"),
    ]:
        run(storage, session, f"INSERT INTO dt VALUES ({i}, '{g}', '{v}')")
    return storage, session


def test_distinct_keeps_rows_a_microsecond_apart(distinct_table):
    """Both halves of the bug: two values collapsed into one, and the survivor
    read `.123000`, which no row held."""
    storage, session = distinct_table
    rows = run(storage, session, "SELECT DISTINCT t FROM dt ORDER BY t").rows
    assert [r[0].microsecond for r in rows] == [122000, 123100, 123500]


def test_distinct_over_several_columns(distinct_table):
    storage, session = distinct_table
    rows = run(storage, session, "SELECT DISTINCT g, t FROM dt ORDER BY g, t").rows
    assert [(r[0], r[1].microsecond) for r in rows] == [
        ("x", 123100),
        ("x", 123500),
        ("y", 122000),
        ("y", 123100),
    ]


def test_distinct_star_keeps_microseconds(distinct_table):
    storage, session = distinct_table
    rows = run(storage, session, "SELECT DISTINCT * FROM dt ORDER BY id").rows
    assert [r[2].microsecond for r in rows] == [123100, 123500, 123100, 122000]


def test_distinct_orders_descending(distinct_table):
    """The `$sort` runs over the composite, so ordering the deduped output has
    to stay correct too."""
    storage, session = distinct_table
    rows = run(storage, session, "SELECT DISTINCT t FROM dt ORDER BY t DESC").rows
    assert [r[0].microsecond for r in rows] == [123500, 123100, 122000]


def test_count_distinct_counts_microsecond_distinct_values(distinct_table):
    """A FIFTH path again: `count(DISTINCT t)` collects an `$addToSet` and
    counts it, so the set has to hold composites. It answered 2 for three
    distinct values."""
    storage, session = distinct_table
    rows = run(storage, session, "SELECT count(DISTINCT t) FROM dt").rows
    assert rows == [(3,)]


def test_distinct_with_where_and_limit(distinct_table):
    storage, session = distinct_table
    rows = run(storage, session, "SELECT DISTINCT t FROM dt WHERE id < 4 ORDER BY t LIMIT 2").rows
    assert [r[0].microsecond for r in rows] == [123100, 123500]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_subms_distinct_matches_real_postgres(distinct_table):
    storage, session = distinct_table
    pg = _pg_oracle()
    assert pg is not None
    try:
        pg.execute("drop table if exists subms_dt_oracle")
        pg.execute("create table subms_dt_oracle (id int, g text, t timestamp)")
        for i, g, v in [
            (1, "x", "2026-08-18 12:00:00.123100"),
            (2, "x", "2026-08-18 12:00:00.123500"),
            (3, "y", "2026-08-18 12:00:00.123100"),
            (4, "y", "2026-08-18 12:00:00.122000"),
        ]:
            pg.execute("insert into subms_dt_oracle values (%s, %s, %s)", (i, g, v))
        for sql in (
            "select distinct t from {} order by t",
            "select distinct g, t from {} order by g, t",
            "select count(distinct t) from {}",
        ):
            ours = [tuple(r) for r in run(storage, session, sql.format("dt")).rows]
            theirs = [tuple(r) for r in pg.execute(sql.format("subms_dt_oracle")).fetchall()]
            assert ours == theirs, sql
    finally:
        pg.close()
