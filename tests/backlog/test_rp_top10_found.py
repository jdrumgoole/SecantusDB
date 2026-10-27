"""Divergences found by the top-ten batch (2026-10-26), each measured on
PostgreSQL 15.19 with the same statements in a rolled-back transaction.

RP-280 .. RP-283 were found while transcribing PostgreSQL's parse analysis
for the rule trees (RP-203); RP-287 is what the GIN and GiST page models
(RP-254) do not cover yet.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


def _in_block(pg: Any, setup: list[str], query: str) -> tuple:
    pg.execute("begin")
    try:
        for s in setup:
            got = pg_answer(pg, s)
            if got[0] == "err":
                return got
        return pg_answer(pg, query)
    finally:
        pg.execute("rollback")


@pytest.mark.backlog_fixed("RP-280")
def test_a_column_functionally_dependent_on_the_grouped_key(pg: Any) -> None:
    """GROUP BY a table's primary key lets its other columns be selected
    (`check_functional_grouping`): PostgreSQL answers the rows."""
    got = _in_block(
        pg,
        [
            "create table rp280_t (id int primary key, name text)",
            "insert into rp280_t values (1, 'a'), (2, 'b')",
        ],
        "select id, name from rp280_t group by id order by id",
    )
    assert got == ("ok", [(1, "a"), (2, "b")])


@pytest.mark.backlog_fixed("RP-280")
def test_a_unique_key_is_no_functional_dependency(pg: Any) -> None:
    """Only a primary key counts: a UNIQUE column grouped by leaves the
    others ungrouped (42803), as on PostgreSQL 15.19."""
    got = _in_block(
        pg,
        ["create table rp280_u (a int unique, b text)"],
        "select a, b from rp280_u group by a",
    )
    assert got[:2] == ("err", "42803")
    assert 'column "rp280_u.b" must appear in the GROUP BY clause' in got[2]


@pytest.mark.backlog_fixed("RP-280")
def test_a_view_leaning_on_the_key_holds_its_constraint(pg: Any) -> None:
    """The view records its dependency on the primary key
    (`constraintDeps`): pg_depend lists it and DROP CONSTRAINT is refused
    while the view stands (2BP01), as on PostgreSQL 15.19."""
    setup = [
        "create table rp280_k (id int primary key, name text)",
        "create view rp280_v as select id, name from rp280_k group by id",
    ]
    got = _in_block(
        pg,
        setup,
        "select count(*) from pg_depend where classid = 'pg_rewrite'::regclass"
        " and refclassid = 'pg_constraint'::regclass and refobjid ="
        " (select oid from pg_constraint where conname = 'rp280_k_pkey')",
    )
    assert got == ("ok", [(1,)])
    got = _in_block(pg, setup, "alter table rp280_k drop constraint rp280_k_pkey")
    assert got[:2] == ("err", "2BP01")
    assert got[2].endswith(
        "cannot drop constraint rp280_k_pkey on table rp280_k because other objects depend on it"
    )


@pytest.mark.backlog_fixed("RP-281")
def test_a_deferrable_primary_key_is_deferrable(pg: Any) -> None:
    """`PRIMARY KEY (a, b) DEFERRABLE` records `condeferrable`."""
    got = _in_block(
        pg,
        ["create table rp281_t (a int, b int, primary key (a, b) deferrable)"],
        "select condeferrable, condeferred from pg_constraint"
        " where conrelid = 'rp281_t'::regclass and contype = 'p'",
    )
    assert got == ("ok", [(True, False)])


@pytest.mark.backlog_fixed("RP-281")
@pytest.mark.parametrize(
    "setup, query, want",
    [
        # Checked at the end of the statement, not row by row.
        (
            [
                "create table rp281_s (a int primary key deferrable, b int)",
                "insert into rp281_s values (1, 1), (2, 2)",
                "update rp281_s set a = a + 1",
            ],
            "select a from rp281_s order by a",
            ("ok", [(2,), (3,)]),
        ),
        (
            [
                "create table rp281_s (a int primary key deferrable)",
                "insert into rp281_s values (1)",
            ],
            "insert into rp281_s values (1)",
            ("err", "23505", 'duplicate key value violates unique constraint "rp281_s_pkey"'),
        ),
        (
            ["create table rp281_s (a int primary key deferrable)"],
            "insert into rp281_s values (null)",
            (
                "err",
                "23502",
                'null value in column "a" of relation "rp281_s" violates not-null constraint',
            ),
        ),
        # INITIALLY DEFERRED: a duplicate stands until COMMIT.
        (
            [
                "create table rp281_s (a int, b int,"
                " primary key (a, b) deferrable initially deferred)",
                "insert into rp281_s values (1, 1), (1, 1)",
            ],
            "select count(*) from rp281_s",
            ("ok", [(2,)]),
        ),
        (
            ["create table rp281_s (a int, constraint rp281_k primary key (a) deferrable)"],
            "select c.contype, pg_get_constraintdef(c.oid), i.indisprimary, i.indimmediate"
            " from pg_constraint c join pg_index i on i.indexrelid = c.conindid"
            " where c.conrelid = 'rp281_s'::regclass",
            ("ok", [("p", "PRIMARY KEY (a) DEFERRABLE", True, False)]),
        ),
        (
            ["create table rp281_s (a int primary key deferrable)"],
            "select constraint_type, is_deferrable, nulls_distinct"
            " from information_schema.table_constraints"
            " where table_name = 'rp281_s' and constraint_type <> 'CHECK'",
            ("ok", [("PRIMARY KEY", "YES", None)]),
        ),
        (
            ["create table rp281_s (a int primary key deferrable)"],
            "create table rp281_r (x int references rp281_s)",
            ("err", "55000", 'cannot use a deferrable primary key for referenced table "rp281_s"'),
        ),
        (
            [
                "create table rp281_s (a int, b int)",
                "insert into rp281_s values (1, 1), (2, 2)",
                "alter table rp281_s add primary key (a) deferrable initially deferred",
                "update rp281_s set a = 1",
            ],
            "select pg_get_constraintdef(oid), condeferred from pg_constraint"
            " where conrelid = 'rp281_s'::regclass and contype = 'p'",
            ("ok", [("PRIMARY KEY (a) DEFERRABLE INITIALLY DEFERRED", True)]),
        ),
        (
            ["create table rp281_s (a int)", "insert into rp281_s values (1), (1)"],
            "alter table rp281_s add primary key (a) deferrable",
            ("err", "23505", 'could not create unique index "rp281_s_pkey"'),
        ),
        (
            [
                "create table rp281_s (a int, b int)",
                "alter table rp281_s add primary key (a) deferrable",
            ],
            "alter table rp281_s add primary key (b)",
            ("err", "42P16", 'multiple primary keys for table "rp281_s" are not allowed'),
        ),
    ],
)
def test_a_deferrable_primary_key_is_checked_as_postgres_checks_it(
    pg: Any, setup: list[str], query: str, want: tuple
) -> None:
    """A DEFERRABLE primary key is judged at statement end (or COMMIT when
    deferred), and the catalogs report it as PostgreSQL 15.19 does."""
    assert _in_block(pg, setup, query) == want


@pytest.mark.backlog_fixed("RP-281")
def test_an_initially_deferred_primary_key_is_judged_at_commit(pg: Any) -> None:
    pg.execute("begin")
    try:
        pg.execute("create table rp281_c (a int primary key deferrable initially deferred)")
        pg.execute("insert into rp281_c values (1), (1)")
        assert pg_answer(pg, "commit") == (
            "err",
            "23505",
            'duplicate key value violates unique constraint "rp281_c_pkey"',
        )
    finally:
        pg.execute("rollback")


@pytest.mark.backlog_fixed("RP-282")
@pytest.mark.parametrize(
    "setup, statement, error",
    [
        (
            ["create table rp282_t (a int, b text)", "create table rp282_l (a int, b text)"],
            "create rule rp282_r as on insert to rp282_t do also insert into rp282_l"
            " values (new.a, new.b) returning *",
            ("0A000", "RETURNING lists are not supported in non-INSTEAD rules"),
        ),
        (
            ["create table rp282_t (a int, b text)", "create table rp282_d (a date)"],
            "create rule rp282_r2 as on delete to rp282_t do also insert into rp282_d select old.*",
            ("42601", "INSERT has more expressions than target columns"),
        ),
        (
            ["create table rp282_g (id int, parent int)"],
            "create view rp282_v as with recursive w(id) as (select id from rp282_g"
            " union all select g.id from rp282_g g join w on g.parent = w.id)"
            " search depth first by id set ord select * from w",
            ("42P16", 'column "ord" has pseudo-type record[]'),
        ),
    ],
    ids=[
        "returning in a non-instead rule",
        "old.* into a narrower table",
        "search column in a view",
    ],
)
def test_rules_and_views_postgres_refuses(
    pg: Any, setup: list[str], statement: str, error: tuple[str, str]
) -> None:
    """Parse analysis refuses each; this server stored the rule or view."""
    got = _in_block(pg, setup, statement)
    assert got[:2] == ("err", error[0])
    assert got[2].endswith(error[1])


_RP282_SETUP = [
    "create table rf_t (a int primary key, b text)",
    "create table rf_l (a int, b text)",
    "create table rf_g (id int, parent int)",
]

# The checks those three cases come from, measured on PostgreSQL 15.19:
# DefineView / DefineVirtualRelation / CheckAttributeNamesTypes,
# transformRuleStmt and DefineQueryRewrite (with checkRuleResultList),
# transformInsertRow, checkWellFormedRecursion and parseCheckAggregates for
# WITH RECURSIVE, analyzeCTE's SEARCH / CYCLE checks, transformLockingClause
# and CheckSelectLocking, select_common_type, transformOnConflictArbiter, and
# collation merging.
_RP282_CASES = [
    (
        "create view rf_v1 as select a, b as a from rf_t",
        "42701",
        'column "a" specified more than once',
    ),
    (
        "create view rf_v2 as select a from rf_t group by a with check option",
        "0A000",
        "WITH CHECK OPTION is supported only on automatically updatable views",
    ),
    (
        "create view rf_v3 as with w as (insert into rf_l values (1, "
        "'x') returning a) select a from "
        "w",
        "0A000",
        "views must not contain data-modifying statements in WITH",
    ),
    (
        "create view rf_v4 (x, y, z) as select a, b from rf_t",
        "42601",
        "CREATE VIEW specifies more column names than columns",
    ),
    (
        "create rule rf_r1 as on insert to rf_t do instead insert into rf_l values (new.a, new.b) "
        "returning a, b, 1",
        "42P17",
        "RETURNING list has too many entries",
    ),
    (
        "create rule rf_r2 as on insert to rf_t do instead insert into rf_l values (new.a, new.b) "
        "returning b, a",
        "42P17",
        'RETURNING list\'s entry 1 has different type from column "a"',
    ),
    (
        "create rule rf_r3 as on insert to rf_t where new.a > 0 do instead insert into rf_l values "
        "(new.a, new.b) returning *",
        "0A000",
        "RETURNING lists are not supported in conditional rules",
    ),
    (
        "create rule rf_r4 as on insert to rf_t do instead (insert "
        "into rf_l values (1) returning *; "
        "insert into rf_l values (2) returning *)",
        "0A000",
        "cannot have multiple RETURNING lists in a rule",
    ),
    (
        "create rule rf_r5 as on update to rf_t where old.a > 0 do also select 1 union select 2",
        "0A000",
        "conditional UNION/INTERSECT/EXCEPT statements are not implemented",
    ),
    (
        "create rule rf_r6 as on update to rf_t do also with w as (select old.a) insert into rf_l "
        "select * from w",
        "0A000",
        "cannot refer to OLD within WITH query",
    ),
    (
        "create rule rf_r7 as on insert to rf_t where new.a > 0 do also notify rf_chan",
        "42P17",
        "rules with WHERE conditions can only have SELECT, INSERT, UPDATE, or DELETE actions",
    ),
    (
        "create rule rf_r8 as on insert to rf_t do also insert into rf_l values (old.a)",
        "42P17",
        "ON INSERT rule cannot use OLD",
    ),
    (
        'create rule "_RETURN" as on insert to rf_t do also insert into rf_l values (new.a)',
        "42P17",
        'non-view rule for "rf_t" must not be named "_RETURN"',
    ),
    (
        "create view rf_v5 as with recursive w(id) as (select id "
        "from rf_g union all select id from "
        "rf_g where id in (select id from w)) select * from w",
        "42P19",
        'recursive reference to query "w" must not appear within a subquery',
    ),
    (
        "create view rf_v6 as with recursive w(id) as (select id from rf_g union all select g.id "
        "from rf_g g left join w on g.parent = w.id) select * from w",
        "42P19",
        'recursive reference to query "w" must not appear within an outer join',
    ),
    (
        "create view rf_v7 as with recursive w(id) as (select 1 "
        "union all select count(*)::int from "
        "w) select * from w",
        "42P19",
        "aggregate functions are not allowed in a recursive query's recursive term",
    ),
    (
        "create view rf_v8 as with recursive w(id) as (select id from rf_g union all select g.id "
        "from rf_g g join w on g.parent = w.id order by 1) select * from w",
        "0A000",
        "ORDER BY in a recursive query is not implemented",
    ),
    (
        "create view rf_v9 as with w as (select 1 as x) search depth "
        "first by x set s select * from "
        "w",
        "42601",
        "WITH query is not recursive",
    ),
    (
        "create view rf_v10 as select a from rf_t for update of nosuch",
        "42P01",
        'relation "nosuch" in FOR UPDATE clause not found in FROM clause',
    ),
    (
        "create view rf_v11 as select count(*) from rf_t for update",
        "0A000",
        "FOR UPDATE is not allowed with aggregate functions",
    ),
    (
        "create view rf_v12 as with recursive w(n) as (select 1 union all select n::text from w) "
        "select * from w",
        "42804",
        "UNION types integer and text cannot be matched",
    ),
    (
        "create rule rf_r9 as on insert to rf_t do also insert into rf_l (a) values (new.a) on "
        "conflict do update set b = 'x'",
        "42601",
        "ON CONFLICT DO UPDATE requires inference specification or constraint name",
    ),
    (
        "create rule rf_r10 as on insert to rf_t do also insert into rf_l (a) values (new.a) on "
        "conflict on constraint nosuch do nothing",
        "42704",
        'constraint "nosuch" for table "rf_l" does not exist',
    ),
    (
        "create view rf_v13 as select interval '1' day as d, 'x' collate \"C\" || ('y' collate "
        '"POSIX") as c',
        "42P21",
        'collation mismatch between explicit collations "C" and "POSIX"',
    ),
]


@pytest.mark.backlog_fixed("RP-282")
@pytest.mark.parametrize("statement, sqlstate, message", _RP282_CASES)
def test_more_of_parse_analysis_refusals(
    pg: Any, statement: str, sqlstate: str, message: str
) -> None:
    """Each statement PostgreSQL's parse analysis (or DefineView /
    DefineQueryRewrite) refuses is refused here, with its SQLSTATE and
    message."""
    got = _in_block(pg, _RP282_SETUP, statement)
    assert got[:2] == ("err", sqlstate), got
    assert got[2].endswith(message), got


@pytest.mark.backlog("RP-295")
@pytest.mark.parametrize(
    "query, sqlstate, message",
    [
        (
            "with recursive w(n) as (select 1 union all select n::text from w) "
            "select * from w limit 3",
            "42804",
            "UNION types integer and text cannot be matched",
        ),
        (
            "with recursive w(id) as (select 1 union all select count(*)::int from w) "
            "select * from w limit 3",
            "42P19",
            "aggregate functions are not allowed in a recursive query's recursive term",
        ),
    ],
    ids=["a type its non-recursive term does not have", "an aggregate in the recursive term"],
)
def test_a_recursive_query_postgres_refuses(
    pg: Any, query: str, sqlstate: str, message: str
) -> None:
    """A plain SELECT's WITH RECURSIVE is planned without these checks:
    the server answers rows (and, without the LIMIT, never finishes), where
    PostgreSQL 15.19 refuses the query."""
    got = _in_block(pg, [], query)
    assert got[:2] == ("err", sqlstate), got
    assert got[2].endswith(message), got


@pytest.mark.backlog("RP-283")
def test_a_hypothetical_set_aggregate_over_two_columns(pg: Any) -> None:
    """`dense_rank(2, 'x') WITHIN GROUP (ORDER BY a, b)` ranks a row of two
    values; over no rows it is 1."""
    got = _in_block(
        pg,
        ["create table rp283_t (a int, b text)"],
        "select dense_rank(2, 'x') within group (order by a, b) from rp283_t",
    )
    assert got == ("ok", [(1,)])


@pytest.mark.backlog("RP-287")
def test_gin_and_gist_indexes_have_their_pages(pg: Any) -> None:
    """PostgreSQL 15.19 sizes these two indexes at 4 and 18 pages; the page
    models know their pages, but `pg_relation_size` reads 0."""
    got = _in_block(
        pg,
        [
            "create table rp287_t (a int[])",
            "insert into rp287_t select array[g % 100, g % 7] from generate_series(1, 2000) g",
            "create index rp287_g on rp287_t using gin (a)",
            "create table rp287_p (p point)",
            "insert into rp287_p select point(g % 50, g / 50) from generate_series(1, 2000) g",
            "create index rp287_s on rp287_p using gist (p)",
        ],
        "select pg_relation_size('rp287_g') / 8192, pg_relation_size('rp287_s') / 8192",
    )
    assert got == ("ok", [(4, 18)])


_RP019_SETUP = [
    "create table rp19_a (a int)",
    "create table rp19_b (b int)",
    "insert into rp19_a values (1), (2), (3)",
    "insert into rp19_b values (1), (2), (3)",
    "create sequence rp19_s",
]


@pytest.mark.backlog_fixed("RP-019")
def test_a_volatile_condition_runs_once_a_row_of_its_table(pg: Any) -> None:
    """PostgreSQL 15.19 runs a base relation's quals in its scan: a
    volatile call over one table runs once for each of its rows, in a join
    too, and is never folded while planning. Here it ran once (folded), or
    once per joined row; the counts are the sequence's after each query."""
    queries = [
        "select count(*) from rp19_a where rp19_a.a > nextval('rp19_s') * 0",
        "select last_value from rp19_s",
        "select count(*) from rp19_a, rp19_b where rp19_a.a > nextval('rp19_s') * 0",
        "select last_value from rp19_s",
        "select count(*) from rp19_a join rp19_b on rp19_a.a = rp19_b.b"
        " where rp19_b.b + nextval('rp19_s') * 0 > 1",
        "select last_value from rp19_s",
    ]
    pg.execute("begin")
    try:
        for s in _RP019_SETUP:
            pg.execute(s)
        got = [pg_answer(pg, q) for q in queries]
    finally:
        pg.execute("rollback")
    assert got == [
        ("ok", [(3,)]),
        ("ok", [(3,)]),
        ("ok", [(9,)]),
        ("ok", [(6,)]),
        ("ok", [(2,)]),
        ("ok", [(9,)]),
    ]


@pytest.mark.backlog("RP-288")
def test_a_select_list_nextval_in_an_autocommit_join_runs_once_a_row(rust_pg: Any) -> None:
    """In autocommit, a join whose select list calls `nextval` took one
    value more than its rows (in a transaction block it did not)."""
    with rust_pg.connect() as conn:
        for s in ["drop table if exists rp19_a, rp19_b", "drop sequence if exists rp19_s"]:
            conn.execute(s)
        for s in _RP019_SETUP:
            conn.execute(s)
        rows = conn.execute(
            "select rp19_a.a, nextval('rp19_s') from rp19_a, rp19_b"
            " where rp19_a.a = rp19_b.b order by 1"
        ).fetchall()
        last = conn.execute("select last_value from rp19_s").fetchone()
        conn.execute("drop table rp19_a, rp19_b")
        conn.execute("drop sequence rp19_s")
    assert (rows, last) == ([(1, 1), (2, 2), (3, 3)], (3,))
