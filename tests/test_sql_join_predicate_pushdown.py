"""Single-table WHERE conjuncts are pushed to the stage that produces their rows.

A comma join whose WHERE conjuncts each constrain one table is a cross product
in disguise (sqllogictest's `select4` is full of them). Matching after the
`$lookup`s materialises the full product; pushing each conjunct into its own
table's stage collapses that to the surviving rows.

The pushdown is only sound for inner joins. `WHERE` runs *after* the join, so a
predicate on the right table of a LEFT JOIN must delete the outer row — pushing
it into the lookup would leave the row with NULLs and keep it. Those cases are
the point of this file, and every expectation was checked against PostgreSQL 14.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


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
def joined(storage, session):
    run(storage, session, "CREATE TABLE ja (x INT8, y INT8)")
    run(storage, session, "CREATE TABLE jb (p INT8, q INT8)")
    run(storage, session, "CREATE TABLE jc (m INT8, n INT8)")
    run(storage, session, "INSERT INTO ja VALUES (1,10),(2,20),(3,30)")
    run(storage, session, "INSERT INTO jb VALUES (1,100),(2,200),(9,900)")
    run(storage, session, "INSERT INTO jc VALUES (1,1000),(5,5000)")
    return storage, session


# Every expectation is PostgreSQL 14's answer for this data.
@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        # The shape being optimised.
        ("SELECT x, p FROM ja, jb WHERE x = 2 AND p = 1 ORDER BY x, p", [(2, 1)]),
        (
            "SELECT x, p, m FROM ja, jb, jc WHERE x = 1 AND p = 1 AND m = 1 ORDER BY 1,2,3",
            [(1, 1, 1)],
        ),
        # An equi-join plus a single-table filter.
        ("SELECT x, q FROM ja, jb WHERE x = p AND y > 10 ORDER BY x", [(2, 200)]),
        # An OR spanning two tables must NOT be pushed into either.
        (
            "SELECT x, p FROM ja, jb WHERE x = 1 OR p = 9 ORDER BY x, p",
            [(1, 1), (1, 2), (1, 9), (2, 9), (3, 9)],
        ),
        # An OR confined to one table still only constrains that table.
        (
            "SELECT x, p FROM ja, jb WHERE (x = 1 OR x = 3) AND p = 2 ORDER BY x, p",
            [(1, 2), (3, 2)],
        ),
        # Negation + IN.
        (
            "SELECT x, p FROM ja, jb WHERE x <> 1 AND p IN (1,9) ORDER BY x, p",
            [(2, 1), (2, 9), (3, 1), (3, 9)],
        ),
        # No WHERE: the whole cross product survives.
        ("SELECT count(*) FROM ja, jb", [(9,)]),
    ],
)
def test_inner_join_shapes_match_postgres(joined, sql, expected):
    storage, session = joined
    assert run(storage, session, sql).rows == expected


class TestLeftJoinIsNotPushedInto:
    """WHERE-after-join semantics: the trap the pushdown must not fall into."""

    def test_where_on_the_right_table_deletes_unmatched_rows(self, joined):
        storage, session = joined
        # Pushing `q = 100` into the lookup would keep x=2 and x=3 with a NULL q.
        assert run(
            storage, session, "SELECT x, q FROM ja LEFT JOIN jb ON x = p WHERE q = 100 ORDER BY x"
        ).rows == [(1, 100)]

    def test_the_same_predicate_in_on_keeps_them(self, joined):
        storage, session = joined
        assert run(
            storage,
            session,
            "SELECT x, q FROM ja LEFT JOIN jb ON x = p AND q = 100 ORDER BY x",
        ).rows == [(1, 100), (2, None), (3, None)]

    def test_a_left_table_filter_is_unaffected(self, joined):
        storage, session = joined
        assert run(
            storage, session, "SELECT x, q FROM ja LEFT JOIN jb ON x = p WHERE x >= 2 ORDER BY x"
        ).rows == [(2, 200), (3, None)]

    def test_anti_join_still_finds_the_unmatched_rows(self, joined):
        storage, session = joined
        assert run(
            storage, session, "SELECT x FROM ja LEFT JOIN jb ON x = p WHERE q IS NULL ORDER BY x"
        ).rows == [(3,)]


def test_a_single_table_or_is_pushed(joined):
    """An OR confined to one table moves; a spanning one cannot.

    sqllogictest's 6-way joins constrain a table with nothing but an OR
    (`e9=245 OR 35=e9 OR 799=e9`). Leaving it behind left that table both
    unfiltered and unjoined, which is what kept the plan exploding.
    """
    import sqlglot

    from secantus.sql import planner
    from secantus.sql.catalog import Catalog

    storage, session = joined
    stmt = sqlglot.parse_one(
        "SELECT x, p FROM ja, jb WHERE (p = 1 OR p = 9) AND x = 1", read="postgres"
    )
    planner.desugar_join_using(stmt)
    plan = planner.plan_pipeline_select(stmt, DB, Catalog(storage), storage)
    pushed = [
        st["$lookup"]["pipeline"] for st in plan.pipeline if st.get("$lookup", {}).get("pipeline")
    ]
    assert pushed, "the single-table OR should have moved into jb's lookup"
    # ... and the prefix is stripped, since the sub-pipeline runs on jb itself.
    assert "jb.p" not in str(pushed)
    # The rows still match PostgreSQL.
    assert run(
        storage, session, "SELECT x, p FROM ja, jb WHERE (p = 1 OR p = 9) AND x = 1"
    ).rows == [
        (1, 1),
        (1, 9),
    ]


def test_the_predicates_land_in_the_early_stages(joined):
    """The plan itself, so a regression that merely stops optimising is caught.

    Without this, the pushdown could silently stop working and every result test
    above would still pass — just slowly.
    """
    import sqlglot

    from secantus.sql import planner
    from secantus.sql.catalog import Catalog

    storage, _session = joined
    stmt = sqlglot.parse_one(
        "SELECT x, p, m FROM ja, jb, jc WHERE x = 1 AND p = 1 AND m = 1", read="postgres"
    )
    planner.desugar_join_using(stmt)
    plan = planner.plan_pipeline_select(stmt, DB, Catalog(storage), storage)

    stages = plan.pipeline
    assert stages[0] == {"$match": {"x": 1}}, "the base table's predicate must precede the lookups"
    pushed = [
        st["$lookup"]["pipeline"][0]["$match"]
        for st in stages
        if "$lookup" in st and st["$lookup"].get("pipeline")
    ]
    assert {"p": 1} in pushed and {"m": 1} in pushed
    # Nothing is left to match after the join for this shape.
    assert not [st for st in stages if "$match" in st and st is not stages[0]]
