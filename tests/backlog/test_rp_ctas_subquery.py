"""Gap test of a subquery in a CREATE TABLE AS or CREATE MATERIALIZED VIEW
select list (found while working RP-449).

Each answer was measured on a private, freshly initdb'd PostgreSQL 15.19
cluster with this module's table, as the shared reference must not be given
objects.
"""

from __future__ import annotations

from typing import Any

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


@pytest.mark.backlog("RP-466")
def test_subquery_in_create_table_as(rust_pg: Any) -> None:
    """`0A000 SubLink is not supported yet` for every one, uncorrelated
    `(select 1)` included; `SELECT ... INTO` of the same list is made."""
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
