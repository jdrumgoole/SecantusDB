"""What was found while working RP-692 (a set operation's group is its
first input's first row).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t692j, t692e cascade",
    "create table t692j (x numeric, k int)",
    "insert into t692j values (1.0, 1)",
    "create table t692e (x numeric, k int)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _text(c: psycopg.Connection, query: str) -> list[tuple[str | None, ...]]:
    return [tuple(None if v is None else str(v) for v in r) for r in c.execute(query).fetchall()]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_join_puts_the_smaller_input_first(dsn: str) -> None:
    """`select a.x from t692j a join t692j b on a.k = b.k intersect select
    1.000` is `1.000` on PostgreSQL 15.19: the join's estimated groups
    outnumber the constant's one, so the constant is the HashSetOp's first
    input and its row is the group's. A join input was not estimated, so
    the left input stayed first and its `1.0` was returned."""
    with _connect(dsn) as c:
        got = _text(c, "select a.x from t692j a join t692j b on a.k = b.k intersect select 1.000")
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_join_with_a_unique_key_puts_the_smaller_input_first(
    dsn: str,
) -> None:
    """A join on a primary key of a table never analysed is estimated from
    the key's row count as its distinct count: `select b.x from t692j a
    join t692p b on a.k = b.k intersect select 1.0` is `1.0`, the
    constant's, on PostgreSQL 15.19."""
    with _connect(dsn) as c:
        c.execute("drop table if exists t692p")
        c.execute("create table t692p (k int primary key, x numeric)")
        c.execute("insert into t692p values (1, 1.00), (2, 2.0)")
        try:
            got = _text(c, "select b.x from t692j a join t692p b on a.k = b.k intersect select 1.0")
        finally:
            c.execute("drop table t692p")
    assert got == [("1.0",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_comma_join_puts_the_smaller_input_first(dsn: str) -> None:
    """The comma form of the join, `select a.x from t692j a, t692j b where
    a.k = b.k intersect select 1.000`, is `1.000` on PostgreSQL 15.19, as
    the ON form is. Its join clause was a WHERE the set operation's input
    estimate did not read, so the left input stayed first."""
    with _connect(dsn) as c:
        got = _text(c, "select a.x from t692j a, t692j b where a.k = b.k intersect select 1.000")
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_grouped_input_puts_the_smaller_input_first(dsn: str) -> None:
    """`select max(x) from t692j group by k intersect select 1.000` is
    `1.000` on PostgreSQL 15.19: the GROUP BY's estimated groups outnumber
    the constant's one. The aggregate input was not estimated, so the
    left input stayed first."""
    with _connect(dsn) as c:
        got = _text(c, "select max(x) from t692j group by k intersect select 1.000")
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_three_table_join_puts_the_smaller_input_first(dsn: str) -> None:
    """`select a.x from t692j a, t692j b, t692j c where a.k = b.k and b.k =
    c.k intersect select 1.000` is `1.000` on PostgreSQL 15.19: the join's
    estimated groups outnumber the constant's one. A join of more than two
    tables was not estimated, so the left input stayed first."""
    with _connect(dsn) as c:
        got = _text(
            c,
            "select a.x from t692j a, t692j b, t692j c where a.k = b.k and b.k = c.k"
            " intersect select 1.000",
        )
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-842")
def test_a_hashed_set_operation_returns_its_groups_in_hash_order(dsn: str) -> None:
    """A HashSetOp returns its groups in its hash table's order
    (`setop_retrieve_hash_table`): `select x, y from (values (1.0, true),
    (1.00, false), (1.000, true)) v(x, y) intersect select x, y from
    (values (1, true), (1, false)) w(x, y)` is `(1, f), (1, t)` on
    PostgreSQL 15.19. The groups came out in the first input's order,
    `(1, t), (1, f)`."""
    with _connect(dsn) as c:
        got = _text(
            c,
            "select x, y from (values (1.0, true), (1.00, false), (1.000, true)) v(x, y)"
            " intersect select x, y from (values (1, true), (1, false)) w(x, y)",
        )
    assert got == [("1", "False"), ("1", "True")]


@pytest.mark.backlog_fixed("RP-842")
def test_a_hashed_set_operation_outputs_each_group_together_in_hash_order(dsn: str) -> None:
    """Measured on PostgreSQL 15.19, both plans a HashSetOp: an EXCEPT ALL
    returns each group's surviving copies together, in the hash table's
    order, and an INTERSECT whose first input repeats a group after its
    last new one grows the table (`SH_INSERT` checks the fill even for a
    key already present). The server returned the first input's order,
    each copy where its row stood."""
    with _connect(dsn) as c:
        except_all = _text(
            c,
            "select x from (values (3), (1), (4), (1), (5), (9), (2), (6), (5), (3), (5)) v(x)"
            " except all select x from (values (5), (1), (8)) w(x)",
        )
        grown = _text(
            c,
            "select x from (values ('a'), ('b'), ('c'), ('d'), ('e'), ('f'), ('g'), ('h'),"
            " ('a')) v(x) intersect select x from (values ('h'), ('g'), ('f'), ('e'), ('d'),"
            " ('c'), ('b'), ('a'), ('z')) w(x)",
        )
    assert [r[0] for r in except_all] == ["9", "1", "5", "5", "2", "4", "6", "3", "3"]
    assert [r[0] for r in grown] == ["g", "h", "a", "e", "d", "c", "b", "f"]


@pytest.mark.backlog_fixed("RP-843")
def test_a_numeric_constant_selected_from_a_from_item_is_inserted(dsn: str) -> None:
    """`insert into t692e select 1.0, g from generate_series(1, 2) g`, the
    same over `(values (3)) v(g)`, and `insert into t692e (x) select 1.5
    from t692j` insert their rows on PostgreSQL 15.19: the constant is
    `numeric`. Each was 42804 `column "x" is of type numeric but expression
    is of type text` here, though `insert into t692e select 1.0, 1` (no
    FROM) was inserted: a select-list constant over a FROM item was typed
    by its decoded value, not its node."""
    inserts = [
        "insert into t692e select 1.0, g from generate_series(1, 2) g",
        "insert into t692e select 1.0, g from (values (3)) v(g)",
        "insert into t692e (x) select 1.5 from t692j",
    ]
    errors: list[tuple[str, str | None]] = []
    with _connect(dsn) as c:
        for q in inserts:
            try:
                c.execute(q)
            except psycopg.Error as e:
                errors.append((q, e.sqlstate))
        got = _text(c, "select x, k from t692e order by k, x")
    assert errors == []
    assert got == [("1.0", "1"), ("1.0", "2"), ("1.0", "3"), ("1.5", None)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_three_table_on_join_puts_the_smaller_input_first(dsn: str) -> None:
    """The ON form of the three-table join, `select a.x from t692j a join
    t692j b on a.k = b.k join t692j c on b.k = c.k intersect select 1.000`,
    is `1.000` on PostgreSQL 15.19, as the comma form is."""
    with _connect(dsn) as c:
        got = _text(
            c,
            "select a.x from t692j a join t692j b on a.k = b.k join t692j c on b.k = c.k"
            " intersect select 1.000",
        )
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_distinct_input_puts_the_smaller_input_first(dsn: str) -> None:
    """`select distinct x from t692j intersect select 1.000` is `1.000` on
    PostgreSQL 15.19: a DISTINCT input's groups are its rows, the
    estimated groups of its select list, which outnumber the constant's
    one. Here it was `1.0`: a DISTINCT input was not estimated."""
    with _connect(dsn) as c:
        got = _text(c, "select distinct x from t692j intersect select 1.000")
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_having_input_puts_the_smaller_input_first(dsn: str) -> None:
    """`select max(x) from t692j group by k having max(x) > 0 intersect
    select 1.000` is `1.000` on PostgreSQL 15.19: `cost_agg` scales the
    groups by the HAVING's selectivity (1/3 for an inequality over an
    aggregate), still more than the constant's one. Here it was `1.0`: an
    aggregate with a HAVING was not estimated."""
    with _connect(dsn) as c:
        got = _text(
            c, "select max(x) from t692j group by k having max(x) > 0 intersect select 1.000"
        )
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
def test_an_intersect_over_a_join_with_a_where_puts_the_smaller_input_first(dsn: str) -> None:
    """`select a.x from t692j a join t692j b on a.k = b.k where a.x > 0
    intersect select 1.000` is `1.000` on PostgreSQL 15.19: the join's
    rows are its sides' scans' rows (the WHERE's default selectivity on
    one) times `eqjoinsel`, its groups more than the constant's one. Here
    it was `1.0`: a join with a WHERE was not estimated."""
    with _connect(dsn) as c:
        got = _text(
            c,
            "select a.x from t692j a join t692j b on a.k = b.k where a.x > 0"
            " intersect select 1.000",
        )
    assert got == [("1.000",)]


@pytest.mark.backlog_fixed("RP-841")
@pytest.mark.parametrize(
    "query",
    [
        "select x from (select x from t692j) s intersect select 1.000",
        "select x from (select x, k from t692j where k > 0 order by k) s intersect select 1.000",
    ],
    ids=["from subquery", "from subquery with a where and an order by"],
)
def test_an_intersect_over_a_from_subquery_puts_the_smaller_input_first(
    dsn: str, query: str
) -> None:
    """Each is `1.000` on PostgreSQL 15.19: the plain subquery is pulled up
    (`pull_up_simple_subquery`) and estimated as the scan it reads. Here it
    was `1.0`: a FROM subquery other than VALUES was not estimated."""
    with _connect(dsn) as c:
        got = _text(c, query)
    assert got == [("1.000",)]


@pytest.mark.backlog("RP-841")
@pytest.mark.parametrize(
    "query",
    [
        "select x * 1 from t692j group by x * 1 intersect select 1.000",
        "select x from t692j group by x having x > 0 intersect select 1.000",
    ],
    ids=["group by expression", "having over a grouping column"],
)
def test_an_intersect_over_another_unestimated_input_puts_the_smaller_input_first(
    dsn: str, query: str
) -> None:
    """Each is `1.000` on PostgreSQL 15.19: the input's estimated groups
    outnumber the constant's one, so the constant is the HashSetOp's first
    input. Here a GROUP BY expression and a HAVING over a
    grouping column are not estimated, so the left input stays first."""
    with _connect(dsn) as c:
        got = _text(c, query)
    assert got == [("1.000",)]
