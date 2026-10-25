"""Batch 60 of the Rust PG server (2026-10-24): each B-tree index's pages
(RP-196) -- what CREATE INDEX builds, how inserts fill, deduplicate and
split its leaves, what VACUUM's index pass leaves -- in `pg_relation_size`
and in the `relpages` and `reltuples` CREATE INDEX, VACUUM and ANALYZE
record; and what was found building it (RP-236).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch60.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_NOAV = "with (autovacuum_enabled = false)"


def _sizes(*indexes: str) -> str:
    """Each index's recorded size and its file's pages."""
    names = ", ".join(f"'{i}'" for i in indexes)
    return (
        "select relname::text, relpages, reltuples, pg_relation_size(oid) / 8192"
        f" from pg_class where relname in ({names}) order by relname"
    )


def _pages(index: str) -> str:
    return f"select pg_relation_size('{index}') / 8192"


SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "CREATE INDEX builds leaves to the fillfactor": (
        "RP-196",
        False,
        [
            f"create table rp60_b (a int, t text) {_NOAV}",
            "insert into rp60_b select g, md5(g::text) from generate_series(1, 367) g",
            "create index rp60_b_a on rp60_b (a)",
            "insert into rp60_b select g, md5(g::text) from generate_series(368, 1101) g",
            "create index rp60_b_a2 on rp60_b (a)",
            "create index rp60_b_t on rp60_b (t)",
            "create index rp60_b_at on rp60_b (a, t)",
            _sizes("rp60_b_a", "rp60_b_a2", "rp60_b_t", "rp60_b_at"),
            "drop table rp60_b",
        ],
    ),
    "CREATE INDEX deduplicates equal keys": (
        "RP-196",
        False,
        [
            f"create table rp60_d (a int) {_NOAV}",
            "insert into rp60_d select g % 100 from generate_series(1, 10000) g",
            "create index rp60_d_a on rp60_d (a)",
            "create index rp60_d_u on rp60_d ((a + 0))",
            _sizes("rp60_d_a"),
            "drop table rp60_d",
        ],
    ),
    "inserts split leaves in key order and out of it": (
        "RP-196",
        False,
        [
            f"create table rp60_g (a int, b int, t text) {_NOAV}",
            "create index rp60_g_a on rp60_g (a)",
            "create index rp60_g_r on rp60_g (b)",
            "create index rp60_g_t on rp60_g (t)",
            "create index rp60_g_d on rp60_g (a desc)",
            "create index rp60_g_ab on rp60_g ((a % 50), a)",
            "insert into rp60_g select g, (g * 7919) % 10007, md5(g::text)"
            " from generate_series(1, 5000) g",
            _pages("rp60_g_a"),
            _pages("rp60_g_r"),
            _pages("rp60_g_t"),
            _pages("rp60_g_d"),
            "drop table rp60_g",
        ],
    ),
    "a composite key in ascending runs splits after the new item": (
        "RP-196",
        False,
        [
            f"create table rp60_c (a int, b int) {_NOAV}",
            "create index rp60_c_ab on rp60_c (a, b)",
            "insert into rp60_c select g % 50, g from generate_series(1, 10000) g",
            _pages("rp60_c_ab"),
            "drop table rp60_c",
        ],
    ),
    "inserted duplicates deduplicate": (
        "RP-196",
        False,
        [
            f"create table rp60_u (a int, b int) {_NOAV}",
            "create index rp60_u_a on rp60_u (a)",
            "create index rp60_u_b on rp60_u (b)",
            "insert into rp60_u select g % 5000, ((g * 7919) % 10007) % 300"
            " from generate_series(1, 20000) g",
            _pages("rp60_u_a"),
            _pages("rp60_u_b"),
            "drop table rp60_u",
        ],
    ),
    "NULLs and INCLUDE columns and a partial index": (
        "RP-196",
        False,
        [
            f"create table rp60_n (a int, b text) {_NOAV}",
            "create index rp60_n_a on rp60_n (a)",
            "create index rp60_n_i on rp60_n (a) include (b)",
            "create index rp60_n_p on rp60_n (a) where a > 2500",
            "insert into rp60_n select case when g % 3 = 0 then null else g end, md5(g::text)"
            " from generate_series(1, 5000) g",
            _pages("rp60_n_a"),
            _pages("rp60_n_i"),
            _pages("rp60_n_p"),
            "drop table rp60_n",
        ],
    ),
    "an index's pg_class row through its life": (
        "RP-196",
        False,
        [
            f"create table rp60_l (a int primary key, b int) {_NOAV}",
            _sizes("rp60_l_pkey"),
            "insert into rp60_l select g, g from generate_series(1, 1000) g",
            _sizes("rp60_l_pkey"),
            "truncate rp60_l",
            _sizes("rp60_l_pkey"),
            "insert into rp60_l select g, g from generate_series(1, 1000) g",
            "vacuum full rp60_l",
            _sizes("rp60_l_pkey"),
            "delete from rp60_l where a <= 300",
            "create index rp60_l_b on rp60_l (b)",
            "create index rp60_l_p on rp60_l (b) where b > 900",
            _sizes("rp60_l_pkey", "rp60_l_b", "rp60_l_p"),
            "analyze rp60_l",
            _sizes("rp60_l_pkey", "rp60_l_b", "rp60_l_p"),
            "vacuum rp60_l",
            _sizes("rp60_l_pkey", "rp60_l_b", "rp60_l_p"),
            "reindex table rp60_l",
            _sizes("rp60_l_pkey", "rp60_l_b", "rp60_l_p"),
            "alter table rp60_l add constraint rp60_l_u unique (b)",
            _sizes("rp60_l_u"),
            "drop table rp60_l",
        ],
    ),
    "VACUUM's index pass removes dead tuples and keeps the pages": (
        "RP-196",
        False,
        [
            f"create table rp60_v (a int, b int) {_NOAV}",
            "insert into rp60_v select g, g from generate_series(1, 1000) g",
            "create index rp60_v_a on rp60_v (a)",
            "create index rp60_v_b on rp60_v (b)",
            "delete from rp60_v where a % 2 = 0",
            "vacuum rp60_v",
            _sizes("rp60_v_a", "rp60_v_b"),
            "update rp60_v set b = b + 1000",
            _sizes("rp60_v_a", "rp60_v_b"),
            "vacuum rp60_v",
            _sizes("rp60_v_a", "rp60_v_b"),
            "insert into rp60_v select g, g from generate_series(3000, 6000) g",
            _pages("rp60_v_b"),
            "drop table rp60_v",
        ],
    ),
    # ------------------------------------------------- RP-196's remainder
    "bottom-up deletion keeps an unchanged key's index small": (
        "RP-196",
        False,
        [
            f"create table rp60_k (a int primary key, b int) {_NOAV}",
            "insert into rp60_k select g, 0 from generate_series(1, 2000) g",
            "update rp60_k set b = b + 1",
            "update rp60_k set b = b + 1",
            "update rp60_k set b = b + 1",
            "update rp60_k set b = b + 1",
            "update rp60_k set b = b + 1",
            _pages("rp60_k_pkey"),
            "drop table rp60_k",
        ],
    ),
    # ------------------------------------- RP-236 a partial index's predicate
    "a partial index over an expression's value": (
        "RP-236",
        False,
        [
            "create table rp60_e (a int)",
            "create index rp60_e_a on rp60_e (a) where a % 2 = 0",
            "insert into rp60_e select generate_series(1, 10)",
            "select count(*) from rp60_e where a % 2 = 0",
            "drop table if exists rp60_e",
        ],
    ),
}


@pytest.mark.backlog_fixed("RP-196")
def test_an_index_keeps_its_pages_across_a_restart(tmp_path: Any) -> None:
    """PostgreSQL 15.19 keeps an index's pages across a restart: 1000 keys
    inserted out of order fill 7 pages (a built index of them has 5), and
    1000 more make 11, restart or not."""
    import psycopg

    from tests.test_rust_pgserver_slice import _Server

    def run(server: Any, statements: list[str]) -> list[Any]:
        out: list[Any] = []
        with psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c:
            for sql in statements:
                cur = c.execute(sql)
                if cur.description is not None:
                    out.append([str(r[0]) for r in cur.fetchall()])
        return out

    with _Server(tmp_path) as server:
        before = run(
            server,
            [
                f"create table rp60_r (a int) {_NOAV}",
                "create index rp60_r_a on rp60_r (a)",
                "insert into rp60_r select (g * 7919) % 10007 from generate_series(1, 1000) g",
                _pages("rp60_r_a"),
            ],
        )
    with _Server(tmp_path) as server:
        after = run(
            server,
            [
                _pages("rp60_r_a"),
                "insert into rp60_r select (g * 7919) % 10007 from generate_series(1001, 2000) g",
                _pages("rp60_r_a"),
            ],
        )
    assert (before, after) == ([["7"]], [["7"], ["11"]])


def _scenario(name: str, server: Any) -> list[Any]:
    return run_scenario(_dsn(server) + " dbname=postgres", SCENARIOS[name][2])


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    assert _scenario(name, rust_pg) == EXPECTED[name]


EXPECTED: dict[str, Any] = {
    "CREATE INDEX builds leaves to the fillfactor": [
        "CREATE TABLE",
        "INSERT 0 367",
        "CREATE INDEX",
        "INSERT 0 734",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        [
            ("rp60_b_a", "2", "367.0", "5"),
            ("rp60_b_a2", "6", "1101.0", "6"),
            ("rp60_b_at", "10", "1101.0", "10"),
            ("rp60_b_t", "10", "1101.0", "10"),
        ],
        "DROP TABLE",
    ],
    "CREATE INDEX deduplicates equal keys": [
        "CREATE TABLE",
        "INSERT 0 10000",
        "CREATE INDEX",
        "CREATE INDEX",
        [("rp60_d_a", "11", "10000.0", "11")],
        "DROP TABLE",
    ],
    "inserts split leaves in key order and out of it": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "INSERT 0 5000",
        [("16",)],
        [("21",)],
        [("47",)],
        [("27",)],
        "DROP TABLE",
    ],
    "a composite key in ascending runs splits after the new item": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 10000",
        [("42",)],
        "DROP TABLE",
    ],
    "inserted duplicates deduplicate": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "INSERT 0 20000",
        [("44",)],
        [("21",)],
        "DROP TABLE",
    ],
    "NULLs and INCLUDE columns and a partial index": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        "INSERT 0 5000",
        [("20",)],
        [("57",)],
        [("7",)],
        "DROP TABLE",
    ],
    "an index's pg_class row through its life": [
        "CREATE TABLE",
        [("rp60_l_pkey", "1", "0.0", "1")],
        "INSERT 0 1000",
        [("rp60_l_pkey", "1", "0.0", "5")],
        "TRUNCATE TABLE",
        [("rp60_l_pkey", "0", "-1.0", "1")],
        "INSERT 0 1000",
        "VACUUM",
        [("rp60_l_pkey", "5", "1000.0", "5")],
        "DELETE 300",
        "CREATE INDEX",
        "CREATE INDEX",
        [
            ("rp60_l_b", "4", "700.0", "4"),
            ("rp60_l_p", "2", "100.0", "2"),
            ("rp60_l_pkey", "5", "1000.0", "5"),
        ],
        "ANALYZE",
        [
            ("rp60_l_b", "4", "700.0", "4"),
            ("rp60_l_p", "2", "100.0", "2"),
            ("rp60_l_pkey", "5", "700.0", "5"),
        ],
        "VACUUM",
        [
            ("rp60_l_b", "4", "700.0", "4"),
            ("rp60_l_p", "2", "100.0", "2"),
            ("rp60_l_pkey", "5", "700.0", "5"),
        ],
        "REINDEX",
        [
            ("rp60_l_b", "4", "700.0", "4"),
            ("rp60_l_p", "2", "100.0", "2"),
            ("rp60_l_pkey", "4", "700.0", "4"),
        ],
        "ALTER TABLE",
        [("rp60_l_u", "4", "700.0", "4")],
        "DROP TABLE",
    ],
    "VACUUM's index pass removes dead tuples and keeps the pages": [
        "CREATE TABLE",
        "INSERT 0 1000",
        "CREATE INDEX",
        "CREATE INDEX",
        "DELETE 500",
        "VACUUM",
        [("rp60_v_a", "5", "500.0", "5"), ("rp60_v_b", "5", "500.0", "5")],
        "UPDATE 500",
        [("rp60_v_a", "5", "500.0", "5"), ("rp60_v_b", "5", "500.0", "6")],
        "VACUUM",
        [("rp60_v_a", "5", "500.0", "5"), ("rp60_v_b", "6", "500.0", "6")],
        "INSERT 0 3001",
        [("14",)],
        "DROP TABLE",
    ],
    "bottom-up deletion keeps an unchanged key's index small": [
        "CREATE TABLE",
        "INSERT 0 2000",
        "UPDATE 2000",
        "UPDATE 2000",
        "UPDATE 2000",
        "UPDATE 2000",
        "UPDATE 2000",
        [("17",)],
        "DROP TABLE",
    ],
    "a partial index over an expression's value": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 10",
        [("5",)],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
