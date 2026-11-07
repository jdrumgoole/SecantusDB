"""Regression tests of a subquery in a CREATE TABLE AS or CREATE MATERIALIZED
VIEW select list (RP-466, found while working RP-449), and of the columns
such a relation lists in `pg_attribute` (RP-484) and the type modifier
each takes from its query (RP-489) -- through a subquery, a CTE, a join, a
set operation and the expressions whose operands share one (RP-493) --
and the system columns it lists for every relation with storage (RP-490,
found while working RP-484). RP-494, found while working RP-489, RP-497,
found while working RP-493, and RP-498, found while working RP-490, are
still open.

Each answer was measured on a private, freshly initdb'd PostgreSQL 15.19
cluster with this module's table, as the shared reference must not be given
objects.
"""

from __future__ import annotations

from decimal import Decimal
from typing import Any

import psycopg
import pytest

# Statement, then the query that reads back what it made, and PostgreSQL
# 15.19's (column names, rows) for that read.
MADE = [
    (
        "create table rp466_ct1 as select (select 1) x from rp466_e",
        "select * from rp466_ct1 order by 1",
        (["x"], [(1,), (1,)]),
    ),
    (
        "create table rp466_ct2 as select a,"
        " exists(select 1 from rp466_e f where f.a = rp466_e.a) from rp466_e",
        "select * from rp466_ct2 order by 1",
        (["a", "exists"], [(1, True), (2, True)]),
    ),
    (
        "create table rp466_ct3 as select (select rp466_e.a + 1) from rp466_e",
        "select * from rp466_ct3 order by 1",
        (["?column?"], [(2,), (3,)]),
    ),
    (
        "create materialized view rp466_mv as select (select a + 1) from rp466_e",
        "select * from rp466_mv order by 1",
        (["?column?"], [(2,), (3,)]),
    ),
]


@pytest.mark.backlog_fixed("RP-466")
def test_subquery_in_create_table_as(rust_pg: Any) -> None:
    """Each was `0A000 SubLink is not supported yet`, uncorrelated `(select
    1)` included, while `SELECT ... INTO` of the same list was made: the
    CREATE's query skipped the pipeline that resolves a SELECT's
    subqueries."""
    got = []
    with rust_pg.connect() as c:
        c.execute("create table rp466_e(a int, b text)")
        c.execute("insert into rp466_e values (1, 'x'), (2, 'y')")
        for make, read, _ in MADE:
            try:
                c.execute(make)
                cur = c.execute(read)
                assert cur.description is not None
                got.append(([d.name for d in cur.description], cur.fetchall()))
            except Exception as e:  # noqa: BLE001 -- the answer IS the error
                got.append(("error", getattr(e, "sqlstate", None), str(e).splitlines()[0]))
    assert got == [want for _, _, want in MADE]


# A statement that fills nothing, then the sequence PostgreSQL 15.19 leaves
# untouched by it: `(last_value, is_called)` after each is (1, False).
FILLS_NOTHING = [
    ("create table rp466_nd as select (select nextval('rp466_s')) n with no data", None),
    (
        "create materialized view rp466_mvnd as select (select nextval('rp466_s')) n with no data",
        None,
    ),
    ("create table if not exists rp466_ex as select (select nextval('rp466_s')) n", None),
    ("create table rp466_ex as select (select nextval('rp466_s')) n", "42P07"),
    ("select (select nextval('rp466_s')) n into rp466_ex", "42P07"),
    ("create materialized view rp466_ex as select (select nextval('rp466_s')) n", "42P07"),
]


@pytest.mark.backlog_fixed("RP-466")
def test_a_create_that_fills_nothing_runs_no_subquery(rust_pg: Any) -> None:
    """`WITH NO DATA` and a relation already there (skipped by `IF NOT
    EXISTS`, 42P07 otherwise) analyse the query but never run it, so its
    subquery draws no sequence value; a `WITH NO DATA` column still takes
    the subquery's type (bigint)."""
    got = []
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create sequence rp466_s")
        c.execute("create table rp466_ex(a int)")
        for make, _ in FILLS_NOTHING:
            state = None
            try:
                c.execute(make)
            except Exception as e:  # noqa: BLE001 -- the answer IS the error
                state = getattr(e, "sqlstate", None)
            got.append((state, c.execute("select last_value, is_called from rp466_s").fetchone()))
        cur = c.execute(
            "select format_type(atttypid, atttypmod) from pg_attribute"
            " where attrelid = 'rp466_nd'::regclass and attnum > 0"
        )
        types = cur.fetchall()
    assert got == [(state, (1, False)) for _, state in FILLS_NOTHING]
    assert types == [("bigint",)]


@pytest.mark.backlog_fixed("RP-466")
def test_refresh_runs_the_subquery_again(rust_pg: Any) -> None:
    """A materialized view keeps its query as written, so REFRESH runs its
    subqueries over the rows there now: PostgreSQL 15.19 answers 7, then
    (1, 1), (2, 2), (5, 3), (7, 4)."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create table rp466_r(a int)")
        c.execute("insert into rp466_r values (1), (2)")
        c.execute("create materialized view rp466_m1 as select (select max(a) from rp466_r) m")
        c.execute(
            "create materialized view rp466_m2 as select a,"
            " (select count(*) from rp466_r f where f.a <= rp466_r.a) c from rp466_r"
        )
        c.execute("insert into rp466_r values (5), (7)")
        c.execute("refresh materialized view rp466_m1")
        c.execute("refresh materialized view rp466_m2")
        m1 = c.execute("select * from rp466_m1").fetchall()
        m2 = c.execute("select * from rp466_m2 order by 1").fetchall()
    assert (m1, m2) == ([(7,)], [(1, 1), (2, 2), (5, 3), (7, 4)])


@pytest.mark.backlog_fixed("RP-484")
def test_matview_columns_listed_once(rust_pg: Any) -> None:
    """Each column of a materialized view is one `pg_attribute` row on a
    private PostgreSQL 15.19 cluster, with data or without; each was two
    here, listed once as the table that holds its rows and again as a
    view."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create materialized view rp_mva as select 1 n, 'x'::text t")
        c.execute("create materialized view rp_mvb as select 1 n with no data")
        rows = c.execute(
            "select attrelid::regclass::text, attname from pg_attribute"
            " where attrelid in ('rp_mva'::regclass, 'rp_mvb'::regclass)"
            " and attnum > 0 order by 1, attnum"
        ).fetchall()
    assert rows == [("rp_mva", "n"), ("rp_mva", "t"), ("rp_mvb", "n")]


@pytest.mark.backlog_fixed("RP-489")
def test_ctas_column_keeps_its_typmod(rust_pg: Any) -> None:
    """A CREATE TABLE AS or CREATE MATERIALIZED VIEW column takes its
    expression's type modifier: `2.5::numeric(5,2)` is `atttypmod` 327686
    on a private PostgreSQL 15.19 cluster, as a declared `numeric(5,2)`
    column is. It was -1 here, the modifier lost, until RP-489."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create table rp489_ct as select 2.5::numeric(5,2) z")
        c.execute("create materialized view rp489_mv as select 2.5::numeric(5,2) z")
        rows = c.execute(
            "select attrelid::regclass::text, atttypmod from pg_attribute"
            " where attrelid in ('rp489_ct'::regclass, 'rp489_mv'::regclass)"
            " and attnum = 1 order by 1"
        ).fetchall()
    assert rows == [("rp489_ct", 327686), ("rp489_mv", 327686)]


# CREATE TABLE AS / MATERIALIZED VIEW sources, then PostgreSQL 15.19's
# `(attname, atttypmod)` for the relation each makes, measured on a private
# cluster over `rp489_b (n numeric(5,2), v varchar(10), ch char(3), t
# timestamp(0))`.
TYPMODS = [
    (
        "create table rp489_c1 as select * from rp489_b",
        [("n", 327686), ("v", 14), ("ch", 7), ("t", 0)],
    ),
    (
        "create table rp489_c2 as select n, ch from rp489_b where n > 0",
        [("n", 327686), ("ch", 7)],
    ),
    (
        "create materialized view rp489_m1 as select n, v, ch from rp489_b",
        [("n", 327686), ("v", 14), ("ch", 7)],
    ),
    (
        "create table rp489_c3 as select 'a'::char(3) ch, 'xy'::varchar(4) v with no data",
        [("ch", 7), ("v", 8)],
    ),
    (
        "create table rp489_c4 as values (2.5::numeric(5,2)), (1.0::numeric(5,2))",
        [("column1", 327686)],
    ),
    (
        "create table rp489_c5 as values (2.5::numeric(5,2)), (1.0::numeric(6,2))",
        [("column1", -1)],
    ),
    (
        "create table rp489_c6 as select now()::timestamp(3) ts, '1 day'::interval(2) iv,"
        " 2.5::numeric(5,2) + 1 sum",
        [("ts", 3), ("iv", 2147418114), ("sum", -1)],
    ),
]


@pytest.mark.backlog_fixed("RP-489")
def test_ctas_typmod_by_source(rust_pg: Any) -> None:
    """Each source's modifier carries into the column it makes -- a base
    table's declared one, a cast's, a VALUES list's common one (-1 when the
    rows disagree), none for an operator's result -- with or without data,
    and survives REFRESH. Measured on a private PostgreSQL 15.19 cluster."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute(
            "create table rp489_b (n numeric(5,2), v varchar(10), ch char(3), t timestamp(0))"
        )
        c.execute("insert into rp489_b values (1.25, 'ab', 'a', '2020-01-01 10:00:00.6')")
        got = []
        for make, _ in TYPMODS:
            c.execute(make)
            rel = make.split()[2] if make.startswith("create table") else make.split()[3]
            if rel == "rp489_m1":
                c.execute("refresh materialized view rp489_m1")
            got.append(
                c.execute(
                    "select attname, atttypmod from pg_attribute"
                    f" where attrelid = '{rel}'::regclass and attnum > 0 order by attnum"
                ).fetchall()
            )
        # The values a typmod'd column holds read back as PostgreSQL's do.
        row = c.execute("select n, ch || '|', octet_length(ch), t::text from rp489_c1").fetchone()
    assert got == [want for _, want in TYPMODS]
    assert row == (Decimal("1.25"), "a|", 3, "2020-01-01 10:00:01")


# Queries whose output column keeps a type modifier on PostgreSQL 15.19 --
# both in its RowDescription and in the column CREATE TABLE AS makes from
# it -- and the modifiers, measured on a private cluster.
_S = "(select 2.5::numeric(5,2) z, 'ab'::varchar(10) v)"
PROPAGATED = [
    (f"select z, v from {_S} s", [327686, 14]),
    (f"with w as {_S} select z, v from w", [327686, 14]),
    (f"select s.z, u.v from {_S} s join {_S} u on true", [327686, 14]),
    (f"select z from {_S} s union all select 3.5::numeric(5,2)", [327686]),
    ("select case when true then 'a'::varchar(3) else 'b'::varchar(3) end a", [7]),
    ("select coalesce(2.5::numeric(5,2), 1::numeric(5,2)) a", [327686]),
    ("select array[1.5::numeric(5,2)] a", [327686]),
    (f"select z from {_S} s group by z", [327686]),
]


@pytest.mark.backlog_fixed("RP-493")
def test_typmod_through_subquery_and_expressions(rust_pg: Any) -> None:
    """A subquery's, a CTE's, a join's or a UNION's column keeps its
    modifier, and so do CASE, COALESCE and ARRAY over operands that share
    one (`exprTypmod`), on a private PostgreSQL 15.19 cluster: the
    RowDescription carries it and CREATE TABLE AS stores it. Each was -1 in
    both until RP-493."""
    described, stored = [], []
    with rust_pg.connect() as c:
        c.autocommit = True
        for i, (q, _) in enumerate(PROPAGATED):
            cur = c.execute(q)
            described.append([cur.pgresult.fmod(k) for k in range(len(cur.description))])
            c.execute(f"create table rp493_{i} as {q}")
            stored.append(
                [
                    m
                    for (m,) in c.execute(
                        "select atttypmod from pg_attribute"
                        f" where attrelid = 'rp493_{i}'::regclass and attnum > 0 order by attnum"
                    ).fetchall()
                ]
            )
    want = [m for _, m in PROPAGATED]
    assert (described, stored) == (want, want)


def _typmod_contexts(c: psycopg.Connection[Any], name: str, q: str) -> dict[str, list[int]]:
    """`q`'s column modifiers as each of PostgreSQL's six readers sees them:
    a portal's Describe and a SELECT from a view over it (the planned
    query), a prepared statement's Describe, the view's own columns and
    CREATE TABLE AS ... WITH NO DATA (the analysed one), and a populating
    CREATE TABLE AS (planned)."""

    def att(rel: str) -> list[int]:
        return [
            m
            for (m,) in c.execute(
                "select atttypmod from pg_attribute"
                f" where attrelid = '{rel}'::regclass and attnum > 0 order by attnum"
            ).fetchall()
        ]

    def mods(r: Any) -> list[int]:
        return [r.fmod(k) for k in range(r.nfields)]

    out = {"portal": mods(c.execute(q).pgresult)}
    c.pgconn.prepare(b"", q.encode())
    out["statement"] = mods(c.pgconn.describe_prepared(b""))
    c.execute(f"create table {name}_t as {q}")
    out["ctas"] = att(f"{name}_t")
    c.execute(f"create table {name}_n as {q} with no data")
    out["ctas_no_data"] = att(f"{name}_n")
    c.execute(f"create view {name}_v as {q}")
    out["view"] = att(f"{name}_v")
    out["from_view"] = mods(c.execute(f"select * from {name}_v").pgresult)
    return out


def _planned(mods: list[int], analysed: list[int]) -> dict[str, list[int]]:
    """The six readers' modifiers: `mods` where PostgreSQL reads the planned
    query, whose constants are folded first, and `analysed` where it reads
    the query as parse analysis leaves it."""
    return {
        "portal": mods,
        "statement": analysed,
        "ctas": mods,
        "ctas_no_data": analysed,
        "view": analysed,
        "from_view": mods,
    }


_W = "'x'::varchar(9) w"
# Queries and their modifiers in each reader, measured on a private
# PostgreSQL 15.19 cluster. `eval_const_expressions` simplifies a CASE with
# a constant WHEN and a COALESCE with a constant argument before the
# planned readers see them, so those differ from the analysed ones.
CONTEXTS = [
    ("select case when true then 'a'::varchar(3) end a", _planned([7], [-1])),
    ("select coalesce(2.5::numeric(5,2), 1::numeric(6,2)) a", _planned([327686], [-1])),
    (
        "select case when false then 'a'::varchar(4) else 'b'::varchar(3) end a",
        _planned([7], [-1]),
    ),
    ("select coalesce(null, 2.5::numeric(5,2)) a", _planned([327686], [-1])),
    (
        f"select case when true then z else 1::numeric(6,2) end a from {_S} s",
        _planned([327686], [-1]),
    ),
    (
        f"select case when z > 1 then z else 1::numeric(5,2) end a from {_S} s",
        _planned([327686], [327686]),
    ),
    (
        f"select coalesce(z, z) a, array[z] b, greatest(z, z) c, z + 1 d from {_S} s",
        _planned([327686, 327686, 327686, -1], [327686, 327686, 327686, -1]),
    ),
    ("values (1::numeric(5,2)), (2::numeric(5,2))", _planned([327686], [327686])),
    ("values (1::numeric(5,2)), (2)", _planned([-1], [-1])),
    ("select 1::numeric(5,2) a union select 2::numeric(5,2)", _planned([327686], [327686])),
    ("select 1::numeric(5,2) a union select 2::numeric(6,2)", _planned([-1], [-1])),
    ("select 1::numeric(5,2) a union select 2", _planned([-1], [-1])),
    (
        f"select * from {_S} s full join (select 2.5::numeric(5,2) z, {_W}) u using (z)",
        _planned([327686, 14, 13], [327686, 14, 13]),
    ),
    (
        f"select * from {_S} s right join (select 2.5::numeric(6,2) z, {_W}) u using (z)",
        _planned([-1, 14, 13], [-1, 14, 13]),
    ),
    ("select current_time(3) a, localtimestamp(2) b", _planned([3, 2], [3, 2])),
    (
        "select greatest(1::numeric(5,2), 2::numeric(5,2)) a, nullif(1::numeric(5,2), 2) b",
        _planned([327686, 327686], [327686, 327686]),
    ),
]


@pytest.mark.backlog_fixed("RP-493")
def test_typmod_planned_and_analysed(rust_pg: Any) -> None:
    """Each reader of a column's modifier sees PostgreSQL 15.19's (a private
    cluster): the planned query's, with a CASE's or a COALESCE's literal
    constants folded first, in a portal's Describe, a populating CREATE
    TABLE AS and a SELECT from a view; the analysed query's in a prepared
    statement's Describe, a view's columns and WITH NO DATA. A USING
    column takes the two sides' common modifier under every join type,
    and a set operation's and a VALUES list's column likewise. Fixed by
    RP-493."""
    with rust_pg.connect() as c:
        c.autocommit = True
        got = [_typmod_contexts(c, f"rp493c{i}", q) for i, (q, _) in enumerate(CONTEXTS)]
    assert got == [want for _, want in CONTEXTS]


@pytest.mark.backlog("RP-497")
def test_typmod_from_scalar_subquery_and_computed_constants(rust_pg: Any) -> None:
    """On a private PostgreSQL 15.19 cluster a scalar subquery's column
    carries its select list's modifier in every reader, and a CASE whose
    WHEN folds to a constant -- a simple CASE over constants, or a
    computed condition -- is simplified before the planned readers see it.
    Here the subquery is -1 everywhere, and only a literal `true` /
    `false` / `null` condition is folded, so the others are -1 in the
    planned readers too."""
    cases = [
        ("select (select 2.5::numeric(5,2)) a", _planned([327686], [327686])),
        ("select case 1 when 1 then 'a'::varchar(3) end a", _planned([7], [-1])),
        ("select case when 1 = 1 then 'a'::varchar(3) end a", _planned([7], [-1])),
    ]
    with rust_pg.connect() as c:
        c.autocommit = True
        got = [_typmod_contexts(c, f"rp497c{i}", q) for i, (q, _) in enumerate(cases)]
    assert got == [want for _, want in cases]


@pytest.mark.backlog("RP-494")
def test_ctas_over_a_bit_column(rust_pg: Any) -> None:
    """`create table t as select B'101'::bit(3) b` makes a `bit(3)` column,
    `atttypmod` 3, on a private PostgreSQL 15.19 cluster; a `varbit` one
    likewise. Here each is refused 0A000 `CREATE TABLE AS over a column of
    type bit is not supported yet`."""
    with rust_pg.connect() as c:
        c.autocommit = True
        got = []
        for name, q in [
            ("rp494_b", "select B'101'::bit(3) b"),
            ("rp494_vb", "select B'10'::varbit(5) b"),
        ]:
            try:
                c.execute(f"create table {name} as {q}")
                got.append(
                    c.execute(
                        "select format_type(atttypid, atttypmod) from pg_attribute"
                        f" where attrelid = '{name}'::regclass and attnum = 1"
                    ).fetchone()
                )
            except psycopg.Error as e:
                got.append((e.sqlstate,))
    assert got == [("bit(3)",), ("bit varying(5)",)]


@pytest.mark.backlog_fixed("RP-490")
def test_pg_attribute_lists_system_columns(rust_pg: Any) -> None:
    """A table's system columns are `pg_attribute` rows with negative
    attnums on a private PostgreSQL 15.19 cluster: `tableoid` -6, `cmax`
    -5, `xmax` -4, `cmin` -3, `xmin` -2, `ctid` -1, each `attnotnull`. A
    materialized view has them too. None was listed, though `select ctid
    from t` answered."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create table rp490_t (a int)")
        c.execute("create materialized view rp490_mv as select 1 n")
        rows = c.execute(
            "select attrelid::regclass::text, attname, atttypid::regtype::text,"
            " attnum, attnotnull from pg_attribute"
            " where attrelid in ('rp490_t'::regclass, 'rp490_mv'::regclass)"
            " and attnum < 0 order by 1, attnum"
        ).fetchall()
    system = [
        ("tableoid", "oid", -6),
        ("cmax", "cid", -5),
        ("xmax", "xid", -4),
        ("cmin", "cid", -3),
        ("xmin", "xid", -2),
        ("ctid", "tid", -1),
    ]
    assert rows == [
        (rel, name, typ, num, True) for rel in ("rp490_mv", "rp490_t") for name, typ, num in system
    ]


@pytest.mark.backlog_fixed("RP-490")
def test_system_columns_by_relation_kind(rust_pg: Any) -> None:
    """Every relation with storage has the six system columns, and a view,
    a composite type and an index have none (`AddNewAttributeTuples` skips
    a view and a composite type; an index is not made through it). Each
    system column's facts are `heap.c`'s `SysAtt`: `ctid` is 6 bytes, not
    by value, `s`-aligned; the rest 4 bytes by value, `i`-aligned; every
    one plain storage with statistics target 0. Measured on a private
    PostgreSQL 15.19 cluster; the statistics target was -1 here, for
    PostgreSQL's own catalogs' system columns too."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create table rp490_k_t (a int)")
        c.execute("create table rp490_k_p (a int) partition by range (a)")
        c.execute("create table rp490_k_p1 partition of rp490_k_p for values from (0) to (10)")
        c.execute("create materialized view rp490_k_mv as select 1 n")
        c.execute("create view rp490_k_v as select 1 x")
        c.execute("create type rp490_k_ct as (x int)")
        c.execute("create index rp490_k_i on rp490_k_t (a)")
        counts = c.execute(
            "select c.relname::text, count(*) filter (where attnum < 0)"
            " from pg_class c join pg_attribute a on a.attrelid = c.oid"
            " where c.relname::text like 'rp490_k_%' group by 1 order by 1"
        ).fetchall()
        facts = c.execute(
            "select attrelid::regclass::text, attname::text, attlen, attbyval,"
            " attalign::text, attstorage::text, attstattarget, atthasdef,"
            " attisdropped, attislocal, attinhcount, attcollation::int8"
            " from pg_attribute where attrelid in"
            " ('rp490_k_p1'::regclass, 'pg_class'::regclass) and attnum < 0"
            " order by attrelid::int8 desc, attnum"
        ).fetchall()
    assert counts == [
        ("rp490_k_ct", 0),
        ("rp490_k_i", 0),
        ("rp490_k_mv", 6),
        ("rp490_k_p", 6),
        ("rp490_k_p1", 6),
        ("rp490_k_t", 6),
        ("rp490_k_v", 0),
    ]
    sysatt = [
        ("tableoid", 4, True, "i"),
        ("cmax", 4, True, "i"),
        ("xmax", 4, True, "i"),
        ("cmin", 4, True, "i"),
        ("xmin", 4, True, "i"),
        ("ctid", 6, False, "s"),
    ]
    assert facts == [
        (rel, name, length, byval, align, "p", 0, False, False, True, 0, 0)
        for rel in ("rp490_k_p1", "pg_class")
        for name, length, byval, align in sysatt
    ]


@pytest.mark.backlog_fixed("RP-498")
def test_pg_attribute_lists_sequence_columns(rust_pg: Any) -> None:
    """A sequence is a relation with storage: on a private PostgreSQL 15.19
    cluster `pg_attribute` lists its three columns and the six system
    columns. Here it listed no row for a sequence at all; it lists all
    nine, read from the same documents `pg_class` lists the sequence from,
    and every pg_attribute column matched 15.19's on a probe."""
    with rust_pg.connect() as c:
        c.autocommit = True
        c.execute("create sequence rp498_s")
        rows = c.execute(
            "select attname::text, attnum, atttypid::regtype::text, attnotnull"
            " from pg_attribute where attrelid = 'rp498_s'::regclass order by attnum"
        ).fetchall()
    assert rows == [
        ("tableoid", -6, "oid", True),
        ("cmax", -5, "cid", True),
        ("xmax", -4, "xid", True),
        ("cmin", -3, "cid", True),
        ("xmin", -2, "xid", True),
        ("ctid", -1, "tid", True),
        ("last_value", 1, "bigint", True),
        ("log_cnt", 2, "bigint", True),
        ("is_called", 3, "boolean", True),
    ]
