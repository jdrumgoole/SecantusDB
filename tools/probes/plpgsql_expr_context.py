"""The CONTEXT of PL/pgSQL expression errors: PostgreSQL vs secantusd-pg.

Each shape is a one-statement PL/pgSQL function that raises; the probe
compares (SQLSTATE, primary message, CONTEXT) from both servers and prints
the shapes that differ. It isolates the `SQL expression "..."` frame, which
PostgreSQL adds only to an error raised while the expression is parsed or
planned (batch 64: 58 of 60 identical against PostgreSQL 15.19).

Usage (a secantusd-pg already listening on PORT)::

    python tools/probes/plpgsql_expr_context.py PORT [--reference DSN]

Self-check: `return 1/0` must carry the frame on the reference server, or
the run aborts (a probe that cannot see the frame proves nothing).
"""

from __future__ import annotations

import argparse
import sys

import psycopg

SHAPES = [
    ("int", "", "return 1/0;"),
    ("int", "x int := 1;", "return x/0;"),
    ("int", "x int := 0;", "return 1/x;"),
    ("int", "x int := 1;", "return 1/0 + x;"),
    ("int", "", "return (select 1/0);"),
    ("int", "x int := 0;", "return (select 1/x);"),
    ("int", "", "return abs(1/0);"),
    ("int", "x int := 1;", "return abs(x/0);"),
    ("int", "", "return (random()*0)::int/0;"),
    ("int", "x int := 1;", "return case when x > 0 then 1/0 end;"),
    ("int", "", "return case when true then 1/0 end;"),
    ("int", "", "return 1/(0)::int;"),
    ("int", "", "return '1'::int/0;"),
    ("int", "", "return 'x'::int;"),
    ("int", "x text := 'a';", "return x::int;"),
    ("int", "t text := 'abc';", "return length(t)/0;"),
    ("int", "y int;", "y := 1/0; return y;"),
    ("int", "y int; x int := 1;", "y := x/0; return y;"),
    ("int", "y int;", "y := (select 1/0); return y;"),
    ("int", "", "if 1/0 > 0 then return 1; end if; return 0;"),
    ("int", "x int := 1;", "if x/0 > 0 then return 1; end if; return 0;"),
    ("int", "", "return (array[1,2])[1]/0;"),
    ("int", "x int;", "return coalesce(x, 1)/0;"),
    ("int", "", "return greatest(1, 2)/0;"),
    ("int", "x int := 1;", "return x + 1/0;"),
    ("int", "", "perform 1/0; return 1;"),
    ("text", "", "return (1/0)::text;"),
    ("int", "", "return 10 % 0;"),
    ("numeric", "", "return 1/0.0;"),
    ("float8", "", "return sqrt(-1);"),
    ("float8", "x float8 := -1;", "return sqrt(x);"),
    ("int", "", "while 1/0 > 0 loop end loop; return 0;"),
    ("int", "", "return 2147483647 + 1;"),
    ("int", "x int := 1;", "return x + 2147483647;"),
    ("int", "", "for i in 1..1/0 loop end loop; return 0;"),
    ("bool", "", "return exists(select 1/0);"),
    ("bool", "", "return 1 in (1/0);"),
    ("text", "", "return format('%s', 1/0);"),
    ("text", "", "return concat('a', 1/0);"),
    ("text", "", "return md5((1/0)::text);"),
    ("text", "", "return lower('A' || (1/0)::text);"),
    ("int", "x constant int := 1;", "return x/0;"),
    ("int", "x int := 1;", "return 'abc'::int + x;"),
    ("int", "", "return (select 1/0 from generate_series(1, 2) g limit 1);"),
    ("int", "", "return 1/0 + (select 1);"),
    ("int", "", "return b64ctx_helper()/0;"),
    ("int", "", "return b64ctx_helper(1/0);"),
    ("int", "", "return b64ctx_immut()/0;"),
    ("int", "x int := 1;", "raise exception 'v %', 1/0;"),
    ("int", "x int := 0;", "raise exception 'v %', 1/x;"),
    ("int", "", "return now()::date - (1/0);"),
    ("int", "", "return 1/0 where true;"),
    ("int[]", "", "return array[1/0];"),
    ("int", "", "return (1, 1/0)::record is not null;"),
    ("int", "", "return -(1/0);"),
    ("int", "", "return 1/0 is null;"),
    ("int", "a int[] := '{1}';", "return a[1]/0;"),
    ("int", "", "return cardinality('{1}'::int[])/0;"),
    ("int", "", "return nullif(1, 2)/0;"),
    ("int", "", "return (1/0) between 1 and 2;"),
]
SETUP = [
    "create or replace function b64ctx_helper(a int default 1) returns int"
    " language plpgsql as $$ begin return a; end $$",
    "create or replace function b64ctx_immut() returns int"
    " language sql immutable as $$ select 1 $$",
]


def run(conn: psycopg.Connection) -> list[tuple[str, str | None, str | None]]:
    out: list[tuple[str, str | None, str | None]] = []
    for s in SETUP:
        conn.execute(s)
    for i, (rt, decl, body) in enumerate(SHAPES):
        name = f"b64ctx_f{i}"
        conn.execute(
            f"create or replace function {name}() returns {rt} language plpgsql"
            f" as $$ declare {decl} begin {body} end $$"
        )
        try:
            conn.execute(f"select {name}()")
            out.append(("OK", None, None))
        except psycopg.Error as e:
            out.append((e.sqlstate or "", e.diag.message_primary, e.diag.context))
        conn.execute(f"drop function {name}()")
    for f in ["b64ctx_helper(int)", "b64ctx_immut()"]:
        conn.execute(f"drop function {f}")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("port", type=int)
    ap.add_argument("--reference", default="host=127.0.0.1 port=5415 user=postgres dbname=postgres")
    args = ap.parse_args()
    ref = psycopg.connect(args.reference, autocommit=True)
    sec = psycopg.connect(
        f"host=127.0.0.1 port={args.port} user=postgres dbname=postgres", autocommit=True
    )
    a, b = run(ref), run(sec)
    if not (a[0][2] or "").startswith('SQL expression "1/0"'):
        print(f"SELF-CHECK FAILED: the reference gave {a[0]!r} for `return 1/0`")
        return 2
    same = 0
    for (_rt, decl, body), x, y in zip(SHAPES, a, b, strict=True):
        if x == y:
            same += 1
            continue
        print(f"--- {decl} {body}\n  PG: {x}\n SEC: {y}")
    print(f"{same} of {len(SHAPES)} identical (state, message, context)")
    return 0 if same == len(SHAPES) else 1


if __name__ == "__main__":
    sys.exit(main())
