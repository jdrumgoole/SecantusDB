"""What working RP-290 (the catalog reads of a join of two catalogs) found:

- a table with a column of a type that can be stored out of line (`text`)
  has no TOAST table: `reltoastrelid` is 0 and `pg_class` holds no
  `pg_toast.pg_toast_<oid>` relation or its index, so a scan of `pg_class`
  returns two rows fewer for each such table than PostgreSQL's
  (RP-1140).

Measured on a private PostgreSQL 15.20 cluster (`initdb -U postgres
--locale=C -E UTF8`): `needs_toast_table` makes one for `rp_toast_t`'s
`text` column (`create_toast_table`, named for the table's oid, in
`pg_toast`, with its `pg_toast_<oid>_index`) and none for `rp_toast_i`,
whose columns are all fixed-width.
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

SETUP = [
    "drop table if exists rp_toast_t, rp_toast_i",
    "create table rp_toast_t (a int, b text)",
    "create table rp_toast_i (a int, b int)",
]

QUERY = """
select c.relname::text, c.reltoastrelid <> 0 as has_toast,
       t.relname = 'pg_toast_' || c.oid as toast_named, t.relkind::text,
       t.relnamespace::regnamespace::text,
       (select i.relname = t.relname || '_index'
          from pg_index x join pg_class i on i.oid = x.indexrelid
         where x.indrelid = t.oid) as index_named
  from pg_class c left join pg_class t on t.oid = c.reltoastrelid
 where c.relname in ('rp_toast_t', 'rp_toast_i') order by 1
"""

EXPECTED = [
    ("rp_toast_i", False, None, None, None, None),
    ("rp_toast_t", True, True, "t", "pg_toast", True),
]


@pytest.mark.backlog("RP-1140")
def test_a_table_with_a_text_column_has_a_toast_table(rust_pg: object) -> None:
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for s in SETUP:
            c.execute(s)
        try:
            assert c.execute(QUERY).fetchall() == EXPECTED
        finally:
            c.execute("drop table if exists rp_toast_t, rp_toast_i")
