"""Gap tests of the divergences found while working RP-860 (a whole-row
reference in a CHECK):

- a column renamed under a CHECK leaves the CHECK naming the old column;
- `pg_get_constraintdef` drops `NOT VALID` from a CHECK added NOT VALID;
- a CHECK comparing `regtype` values prints without the `oid` casts.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each script runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t860 cascade",
    "create table t860 (x int)",
]

_DEF = "select pg_get_constraintdef(oid) from pg_constraint where conrelid = 't860'::regclass"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _script(dsn: str, statements: list[str]) -> list[object]:
    """Each statement's status, rows or error, after `_SETUP`."""
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for q in statements:
            try:
                cur = c.execute(q)
            except psycopg.Error as e:
                out.append(("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position))
                continue
            out.append(cur.fetchall() if cur.description else cur.statusmessage)
    return out


@pytest.mark.backlog_fixed("RP-939")
def test_a_renamed_column_is_renamed_in_its_check(dsn: str) -> None:
    """A CHECK holds the column itself, not its name: after `rename column
    x to y` it still tests the column, now printed `y`, on PostgreSQL
    15.19. The stored text still named `x`, so every INSERT was 42703
    `column "x" does not exist`."""
    assert _script(
        dsn,
        [
            "alter table t860 add constraint k check (x > 0)",
            "alter table t860 rename column x to y",
            "insert into t860 values (1)",
            "insert into t860 values (0)",
            _DEF,
        ],
    ) == [
        "ALTER TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        ("error", "23514", 'new row for relation "t860" violates check constraint "k"', None),
        [("CHECK ((y > 0))",)],
    ]


@pytest.mark.backlog_fixed("RP-940")
def test_a_not_valid_check_prints_not_valid(dsn: str) -> None:
    """`pg_get_constraintdef` ends a CHECK that is not validated with ` NOT
    VALID` on PostgreSQL 15.19. It was left off."""
    assert _script(
        dsn,
        ["alter table t860 add constraint k check (x > 0) not valid", _DEF],
    ) == ["ALTER TABLE", [("CHECK ((x > 0)) NOT VALID",)]]


@pytest.mark.backlog_fixed("RP-941")
def test_a_regtype_comparison_in_a_check_prints_its_oid_casts(dsn: str) -> None:
    """`regtype` has no `=` of its own: the parser compares two of them as
    `oid` (`oideq` over binary-coercible casts), and ruleutils prints the
    casts, `((pg_typeof(x))::oid = ('integer'::regtype)::oid)`, on
    PostgreSQL 15.19. It was printed as written, `(pg_typeof(x) =
    'int4'::regtype)`, until RP-941."""
    assert _script(
        dsn,
        ["alter table t860 add constraint k check (pg_typeof(x) = 'int4'::regtype)", _DEF],
    ) == ["ALTER TABLE", [("CHECK (((pg_typeof(x))::oid = ('integer'::regtype)::oid))",)]]


@pytest.mark.backlog_fixed("RP-1024")
def test_a_renamed_partition_key_column_still_routes(dsn: str) -> None:
    """The partition key holds the column's attnum: after `rename column x
    to y` on a partitioned table, a row still routes and the key prints
    `LIST (y)` on PostgreSQL 15.19 (shared reference, port 5415). The
    stored key text still named `x`, so the INSERT was 42703 `column "x"
    does not exist`. Found while working RP-939."""
    assert _script(
        dsn,
        [
            "drop table if exists p939k",
            "create table p939k (x int) partition by list (x)",
            "create table p939k1 partition of p939k for values in (1)",
            "alter table p939k rename column x to y",
            "insert into p939k values (1)",
            "select pg_get_partkeydef('p939k'::regclass)",
            "drop table p939k",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "CREATE TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        [("LIST (y)",)],
        "DROP TABLE",
    ]


@pytest.mark.backlog_fixed("RP-1052")
def test_pg_get_partkeydef_of_an_expression_key(dsn: str) -> None:
    """`pg_get_partkeydef` prints an expression key as ruleutils does,
    `RANGE (((x + 1)))` on PostgreSQL 15.19 (shared reference, port 5415):
    the key list's parentheses, the expression's own, and the OpExpr's.
    One layer was missing, `RANGE ((x + 1))`. Found while working
    RP-1024."""
    assert _script(
        dsn,
        [
            "drop table if exists p1024e",
            "create table p1024e (x int) partition by range ((x + 1))",
            "select pg_get_partkeydef('p1024e'::regclass)",
            "drop table p1024e",
        ],
    ) == ["DROP TABLE", "CREATE TABLE", [("RANGE (((x + 1)))",)], "DROP TABLE"]


@pytest.mark.backlog_fixed("RP-1053")
def test_a_renamed_column_is_renamed_in_a_generated_column(dsn: str) -> None:
    """A generated column's expression holds the column's attnum: after
    `rename column x to y` an INSERT still computes it, `(3, 6)`, on
    PostgreSQL 15.19 (shared reference, port 5415). The stored expression
    still named `x`, so the INSERT was 42703. Found while working
    RP-1024."""
    assert _script(
        dsn,
        [
            "drop table if exists p1024g",
            "create table p1024g (x int, g int generated always as (x * 2) stored)",
            "alter table p1024g rename column x to y",
            "insert into p1024g (y) values (3)",
            "select * from p1024g",
            "drop table p1024g",
        ],
    ) == ["DROP TABLE", "CREATE TABLE", "ALTER TABLE", "INSERT 0 1", [(3, 6)], "DROP TABLE"]


@pytest.mark.backlog_fixed("RP-1053")
def test_a_rename_to_a_quoted_name_is_printed_in_a_generated_column(dsn: str) -> None:
    """The renamed expression prints quoted where the new name needs it,
    in `information_schema.columns.generation_expression` and
    `pg_get_expr` of its `pg_attrdef` row, and still computes:
    `(("Big Y" * 2) + z)` and `(3, 1, 7)` on PostgreSQL 15.19 (shared
    reference, port 5415)."""
    assert _script(
        dsn,
        [
            "drop table if exists q1053",
            'create table q1053 ("X" int, z int, g int generated always as ("X" * 2 + z) stored)',
            'alter table q1053 rename column "X" to "Big Y"',
            "select generation_expression from information_schema.columns"
            " where table_name = 'q1053' and column_name = 'g'",
            "select pg_get_expr(adbin, adrelid) from pg_attrdef where adrelid = 'q1053'::regclass",
            'insert into q1053 ("Big Y", z) values (3, 1)',
            "select * from q1053",
            "drop table q1053",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "ALTER TABLE",
        [('(("Big Y" * 2) + z)',)],
        [('(("Big Y" * 2) + z)',)],
        "INSERT 0 1",
        [(3, 1, 7)],
        "DROP TABLE",
    ]


@pytest.mark.backlog_fixed("RP-1054")
def test_a_renamed_column_is_renamed_in_an_expression_index(dsn: str) -> None:
    """An index expression and predicate hold the column's attnum: after
    `rename column x to y` an INSERT still maintains the index, which
    prints `(((y + 1))) WHERE (y > 0)`, on PostgreSQL 15.19 (shared
    reference, port 5415). Both used to still name `x`, and the INSERT was
    42703. Found while working RP-1024."""
    assert _script(
        dsn,
        [
            "drop table if exists p1024i",
            "create table p1024i (x int)",
            "create index p1024ii on p1024i ((x + 1)) where x > 0",
            "alter table p1024i rename column x to y",
            "insert into p1024i values (3)",
            "select pg_get_indexdef('p1024ii'::regclass)",
            "drop table p1024i",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "CREATE INDEX",
        "ALTER TABLE",
        "INSERT 0 1",
        [("CREATE INDEX p1024ii ON public.p1024i USING btree (((y + 1))) WHERE (y > 0)",)],
        "DROP TABLE",
    ]


@pytest.mark.backlog_fixed("RP-1054")
def test_a_renamed_column_is_renamed_in_every_index_that_names_it(dsn: str) -> None:
    """A plain key, its INCLUDE and WHERE, a key expression beside a
    function key, and two UNIQUE indexes (an expression one and a partial
    plain one) follow three renames, one to a name that needs quoting.
    Both UNIQUE ones still refuse a duplicate, and a literal naming a
    renamed column is left alone. Measured on PostgreSQL 15.19 (shared
    reference, port 5415). The plain key's storage index was keyed on the
    old field, so it printed NULL and the UNIQUE one enforced nothing."""
    out = _script(
        dsn,
        [
            "drop table if exists r1054",
            "create table r1054 (x int, z text, w int, v int)",
            "create index r1054a on r1054 (w desc) include (v) where x > 0",
            "create index r1054b on r1054 (lower(z), (x + w)) where z <> 'x'",
            "create unique index r1054c on r1054 ((x * 2))",
            "create unique index r1054d on r1054 (w) where w > 0",
            'alter table r1054 rename column x to "Big Y"',
            "alter table r1054 rename column w to ww",
            "alter table r1054 rename column v to vv",
            "insert into r1054 values (3, 'A', 4, 1)",
            "insert into r1054 values (3, 'B', 5, 1)",
            "insert into r1054 values (4, 'B', 4, 1)",
            "select pg_get_indexdef(indexrelid) from pg_index"
            " where indrelid = 'r1054'::regclass order by indexrelid",
            'select "Big Y", ww, vv from r1054 where ww = 4',
            "drop table r1054",
        ],
    )
    assert out[:10] == ["DROP TABLE", "CREATE TABLE"] + ["CREATE INDEX"] * 4 + [
        "ALTER TABLE"
    ] * 3 + ["INSERT 0 1"]
    assert [e[:3] for e in out[10:12]] == [
        ("error", "23505", 'duplicate key value violates unique constraint "r1054c"'),
        ("error", "23505", 'duplicate key value violates unique constraint "r1054d"'),
    ]
    on = "ON public.r1054 USING btree"
    assert out[12:] == [
        [
            (f'CREATE INDEX r1054a {on} (ww DESC) INCLUDE (vv) WHERE ("Big Y" > 0)',),
            (f"CREATE INDEX r1054b {on} (lower(z), ((\"Big Y\" + ww))) WHERE (z <> 'x'::text)",),
            (f'CREATE UNIQUE INDEX r1054c {on} ((("Big Y" * 2)))',),
            (f"CREATE UNIQUE INDEX r1054d {on} (ww) WHERE (ww > 0)",),
        ],
        [(3, 4, 1)],
        "DROP TABLE",
    ]


@pytest.mark.backlog("RP-1082")
def test_pg_get_indexdef_quotes_a_key_and_an_include_column(dsn: str) -> None:
    """ruleutils prints a key column and an INCLUDE column through
    `quote_identifier`, on PostgreSQL 15.19 (shared reference, port 5415).
    Here both are printed bare. Found while working RP-1054."""
    assert _script(
        dsn,
        [
            "drop table if exists q1054",
            'create table q1054 ("Big Y" int, "W w" int)',
            'create index q1054a on q1054 ("Big Y" desc) include ("W w")',
            "select pg_get_indexdef('q1054a'::regclass)",
            "drop table q1054",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "CREATE INDEX",
        [('CREATE INDEX q1054a ON public.q1054 USING btree ("Big Y" DESC) INCLUDE ("W w")',)],
        "DROP TABLE",
    ]


@pytest.mark.backlog("RP-1091")
def test_pg_get_partkeydef_keeps_a_key_collation_and_opclass(dsn: str) -> None:
    """A partition key's COLLATE and operator class are part of the key:
    `partition by range ((x::text) collate "C")` prints `RANGE (((x)::text)
    COLLATE "C")`, and `((x::text) text_pattern_ops)` prints `RANGE
    (((x)::text) text_pattern_ops)`, on PostgreSQL 15.19 (shared reference,
    port 5415). Here both print `RANGE (((x)::text))`. Found while working
    RP-1052."""
    assert _script(
        dsn,
        [
            "drop table if exists p1052c",
            'create table p1052c (x int) partition by range ((x::text) collate "C")',
            "select pg_get_partkeydef('p1052c'::regclass)",
            "drop table p1052c",
            "create table p1052c (x int) partition by range ((x::text) text_pattern_ops)",
            "select pg_get_partkeydef('p1052c'::regclass)",
            "drop table p1052c",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        [('RANGE (((x)::text) COLLATE "C")',)],
        "DROP TABLE",
        "CREATE TABLE",
        [("RANGE (((x)::text) text_pattern_ops)",)],
        "DROP TABLE",
    ]


@pytest.mark.backlog("RP-1092")
def test_client_min_messages_withholds_a_notice(dsn: str) -> None:
    """`set client_min_messages = warning` withholds every NOTICE from the
    client: neither `drop table if exists` of a missing table nor a DO
    block's `raise notice` sends one on PostgreSQL 15.19 (shared reference,
    port 5415), while `raise warning` still does. Here both NOTICEs arrive.
    Found while working RP-1052."""
    seen: list[tuple[str, str]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: seen.append((d.severity or "", d.message_primary or "")))
        c.execute("set client_min_messages = warning")
        c.execute("drop table if exists p1052_missing")
        c.execute("do $$ begin raise notice 'n1052'; raise warning 'w1052'; end $$")
    assert seen == [("WARNING", "w1052")]
