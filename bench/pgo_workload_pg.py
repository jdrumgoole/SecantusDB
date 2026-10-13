"""Drive representative statements against a running `secantusd-pg` to collect
a PGO profile.

The PostgreSQL-side twin of ``bench/pgo_workload.py``. The two-stage release
build (``scripts/build_pg_release.sh``, called by
``.github/workflows/release-pg-binaries.yml``) starts the instrumented binary,
runs this against it, then stops the binary so it writes its profile.

What it runs is what a test suite sends: single-row inserts, selects and
updates by primary key through psycopg's extended protocol, each its own
commit, from one connection and then from eight at once. Those are the
statements `bench/do_pg_bench.py` measures. Around them go the statements a
suite also sends in smaller numbers: DDL, a multi-row insert, a join, an
aggregate, a window, a transaction with a savepoint and `COPY`.

Standalone (psycopg only), so it runs in the minimal release-build CI image.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor

import psycopg


def _point_statements(dsn: str, slot: str, rows: int) -> None:
    """Inserts, selects and updates by primary key, one commit each."""
    table = f"pgo_{slot}"
    with psycopg.connect(dsn, autocommit=True) as conn, conn.cursor() as cur:
        cur.execute(f"drop table if exists {table}")
        cur.execute(f"create table {table} (k bigint primary key, v bigint, s text)")
        cur.execute(f"insert into {table} values (0, 0, 'x')")
        for i in range(1, rows + 1):
            cur.execute(f"insert into {table} values (%s, %s, %s)", (i, i, "row"))
            cur.execute(f"select v from {table} where k = %s", (i,))
            cur.fetchone()
            cur.execute(f"update {table} set v = v + 1 where k = %s", (0,))
            cur.execute(f"select v, s from {table} where k = %s", (0,))
            cur.fetchone()
        cur.execute(f"select count(*), sum(v) from {table}")
        cur.fetchone()


def _suite_statements(dsn: str, reps: int) -> None:
    """The rest of what a test suite sends, in small numbers."""
    with psycopg.connect(dsn, autocommit=True) as conn, conn.cursor() as cur:
        for r in range(reps):
            cur.execute("drop table if exists pgo_item")
            cur.execute("drop table if exists pgo_owner")
            cur.execute("create table pgo_owner (id int primary key, name text not null)")
            cur.execute(
                "create table pgo_item (id int primary key, owner int references pgo_owner (id), "
                "price numeric(10, 2), made timestamptz default now(), tags text[])"
            )
            cur.execute("create index pgo_item_owner on pgo_item (owner)")
            cur.executemany(
                "insert into pgo_owner values (%s, %s)", [(i, f"owner-{i}") for i in range(20)]
            )
            cur.execute(
                "insert into pgo_item (id, owner, price, tags) "
                "select g, g % 20, g * 1.25, array['a', 'b'] from generate_series(1, 400) g"
            )
            cur.execute(
                "select o.name, count(*), sum(i.price) from pgo_item i "
                "join pgo_owner o on o.id = i.owner where i.price > %s "
                "group by o.name order by 2 desc, 1 limit 5",
                (10,),
            )
            cur.fetchall()
            cur.execute(
                "select id, price, row_number() over (partition by owner order by price desc) "
                "from pgo_item where owner < %s",
                (5,),
            )
            cur.fetchall()
            cur.execute("select * from pgo_item where owner = %s order by id", (r % 20,))
            cur.fetchall()
            with conn.transaction():
                cur.execute("update pgo_item set price = price + 1 where owner = %s", (3,))
                try:
                    with conn.transaction():
                        cur.execute("insert into pgo_owner values (1, 'duplicate')")
                except psycopg.errors.UniqueViolation:
                    pass
                cur.execute("delete from pgo_item where id > %s", (300,))
            with cur.copy("copy pgo_owner (id, name) from stdin") as copy:
                for i in range(100, 200):
                    copy.write_row((i, f"copied-{i}"))
            with cur.copy("copy pgo_owner to stdout") as copy:
                for _ in copy:
                    pass
            cur.execute("select table_name from information_schema.tables")
            cur.fetchall()


def run(dsn: str, rows: int, reps: int) -> None:
    _suite_statements(dsn, reps)
    for clients in (1, 8):
        # A pool, so a connection that fails fails the build's workload step.
        with ThreadPoolExecutor(clients) as pool:
            done = [
                pool.submit(_point_statements, dsn, f"{clients}_{slot}", rows)
                for slot in range(clients)
            ]
            for future in done:
                future.result()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dsn", required=True, help="e.g. 'host=127.0.0.1 port=5433 user=postgres'")
    ap.add_argument("--rows", type=int, default=20000, help="point statements per connection")
    ap.add_argument("--reps", type=int, default=20, help="passes of the suite statements")
    args = ap.parse_args()
    run(args.dsn, args.rows, args.reps)
    print("pgo workload complete")


if __name__ == "__main__":
    main()
