"""The Rust PostgreSQL server, diffed against a real PostgreSQL.

The Python SQL server is the *behaviour* oracle; live PostgreSQL is the
*correctness* one, and where they disagree PostgreSQL wins. Every case here runs
the identical DDL, DML and query against both servers and asserts the answers
match — which is how the NULL-ordering and three-valued-logic rules in
`secantus-pgplan` were derived rather than guessed.

Needs BOTH:
  * `secantusd-pg` built  — cd crates/secantus-pgserver && cargo build
  * a live PostgreSQL     — SECANTUS_PG_ORACLE_DSN, default the local 14
"""

from __future__ import annotations

import contextlib
import datetime as dt
import os
import time
from collections.abc import Iterator
from decimal import Decimal
from pathlib import Path

import pytest

psycopg = pytest.importorskip("psycopg")
from psycopg.types.multirange import Multirange  # noqa: E402
from psycopg.types.range import Range  # noqa: E402

from tests.test_rust_pgserver_slice import BINARY, _Server  # noqa: E402

ORACLE_DSN = os.environ.get(
    "SECANTUS_PG_ORACLE_DSN",
    "host=127.0.0.1 port=5432 dbname=postgres user=jdrumgoole",
)


# Short and few on purpose. The failure mode here is a HANG, not a blip:
# Postgres.app gates connections per-application behind a macOS permission
# dialog, so an unapproved process (every pytest-xdist worker) waits for a
# dialog nobody answers until the timeout expires. A generous 15s x 5 budget
# therefore bought nothing and cost 75s of dead waiting per worker.
_CONNECT_TIMEOUT_S = 5
_CONNECT_ATTEMPTS = 2
# Why the last connection attempt failed. A bare "no oracle" skip is
# indistinguishable from "PostgreSQL is not installed", which is how ~100
# silently skipped tests looked like an intentional configuration.
_LAST_ERROR: list[str] = ["never attempted"]


def _oracle() -> psycopg.Connection | None:
    """Connect to the oracle, retrying once on a transient failure.

    **If this skips under the full suite but passes standalone, the cause is
    almost certainly Postgres.app\'s per-application permission gate**, not
    your code and not load. Measured 2026-08-31: every worker got

        FATAL: Postgres.app failed to verify "trust" authentication
        DETAIL: You did not confirm the permission dialog.

    surfacing through psycopg as a bare `ConnectionTimeout`, because the server
    waits on a dialog no background process can answer. It silently disabled
    ~109 tests here and the five pre-existing oracle suites
    (`test_sql_search_path.py` and friends) alongside them. Fix it in
    Postgres.app\'s settings, or point `SECANTUS_PG_ORACLE_DSN` at a plain
    PostgreSQL; there is nothing to fix in the test.
    """
    delay = 0.5
    for attempt in range(_CONNECT_ATTEMPTS):
        try:
            return psycopg.connect(ORACLE_DSN, autocommit=True, connect_timeout=_CONNECT_TIMEOUT_S)
        except Exception as exc:  # noqa: BLE001 - retry, then record why
            _LAST_ERROR[0] = f"{type(exc).__name__}: {exc}"
            if attempt == _CONNECT_ATTEMPTS - 1:
                return None
            time.sleep(delay)
            delay *= 2
    return None


def _oracle_available() -> bool:
    """Probe for the oracle, closing the probe's connection.

    The docstring here used to say the first cut "leaked" a connection per
    worker by calling `_oracle()` straight into a `skipif`. **Measured
    2026-08-31: it did not.** CPython refcounting collects the unreferenced
    connection the moment the comparison is computed, and psycopg closes it on
    `__del__`; nine such probes leave zero rows in `pg_stat_activity`, where
    nine held references leave nine. Closing explicitly is still right -- it
    does not depend on refcounting -- but connection exhaustion was never the
    reason these suites skip. See `tests/pg_oracle.py`, the shared probe the
    six older oracle suites now use, for what the reason actually is.
    """
    conn = _oracle()
    if conn is None:
        return False
    conn.close()
    return True


pytestmark = [
    pytest.mark.skipif(
        not BINARY.exists(),
        reason="secantusd-pg not built (cargo build in crates/secantus-pgserver)",
    ),
    pytest.mark.skipif(
        not _oracle_available(),
        reason=f"no local PostgreSQL oracle ({ORACLE_DSN}): {_LAST_ERROR[0]}",
    ),
]

# One shared fixture table. `n` and `s` are nullable ON PURPOSE: three-valued
# logic is where SQL and MQL diverge, so every case gets a NULL to trip over.
SETUP = [
    "CREATE TABLE d (id int PRIMARY KEY, n int, s text)",
    "INSERT INTO d VALUES (1, 3, 'c'), (2, NULL, 'a'), (3, 1, NULL), (4, 2, 'b')",
]

QUERIES = [
    # --- ORDER BY: null placement is the trap --------------------------------
    "SELECT id FROM d ORDER BY n",
    "SELECT id FROM d ORDER BY n DESC",
    "SELECT id FROM d ORDER BY n ASC NULLS FIRST",
    "SELECT id FROM d ORDER BY n DESC NULLS LAST",
    "SELECT id FROM d ORDER BY s",
    "SELECT id FROM d ORDER BY s DESC",
    "SELECT id FROM d ORDER BY n, id",
    "SELECT id FROM d ORDER BY id DESC",
    # --- LIMIT / OFFSET ------------------------------------------------------
    "SELECT id FROM d ORDER BY id LIMIT 2",
    "SELECT id FROM d ORDER BY id OFFSET 1",
    "SELECT id FROM d ORDER BY id LIMIT 2 OFFSET 1",
    "SELECT id FROM d ORDER BY id LIMIT 0",
    "SELECT id FROM d ORDER BY id OFFSET 99",
    "SELECT id FROM d ORDER BY n LIMIT 3",
    # --- three-valued logic --------------------------------------------------
    "SELECT id FROM d WHERE n IS NULL",
    "SELECT id FROM d WHERE n IS NOT NULL",
    "SELECT id FROM d WHERE s IS NULL",
    "SELECT id FROM d WHERE n IN (1, 3)",
    "SELECT id FROM d WHERE n IN (1, NULL)",
    "SELECT id FROM d WHERE n NOT IN (1)",
    "SELECT id FROM d WHERE n NOT IN (1, NULL)",
    "SELECT id FROM d WHERE s IN ('a', 'c')",
    "SELECT id FROM d WHERE n BETWEEN 1 AND 2",
    "SELECT id FROM d WHERE n NOT BETWEEN 1 AND 2",
    "SELECT id FROM d WHERE n = 1",
    # `= NULL` is never true in SQL -- only `IS NULL` matches. MQL's `{n: null}`
    # would match, so these pin the short-circuit for LITERAL nulls too.
    "SELECT id FROM d WHERE n = NULL",
    "SELECT id FROM d WHERE n <> NULL",
    "SELECT id FROM d WHERE n > NULL",
    "SELECT id FROM d WHERE s = NULL",
    "SELECT id FROM d WHERE n <> 1",
    "SELECT id FROM d WHERE n > 1 AND s IS NOT NULL",
    "SELECT id FROM d WHERE n IS NULL OR n > 2",
    # --- projection ----------------------------------------------------------
    "SELECT id, n, s FROM d ORDER BY id",
    "SELECT s AS label FROM d ORDER BY id",
    # --- casts. The declared type matters as much as the value: `Describe`
    # runs before `Bind`, so a type inferred from the (NULL) value typed
    # `$1::int` as varchar and the client decoded an integer as a string.
    # --- constant expressions. `7/2` is 3 (integer division truncates) and
    # `5/0` is 22012 -- both probed, neither guessed.
    # --- session settings (GUCs). Column CASING is part of the contract:
    # `SHOW datestyle` answers a column called `DateStyle`.
    "SHOW client_encoding",
    "SHOW datestyle",
    "SHOW standard_conforming_strings",
    "SHOW transaction_read_only",
    "SELECT current_setting('client_encoding')",
    "SELECT current_setting('nope.zz', true)",
    "SELECT 1+1",
    "SELECT 1-2",
    "SELECT 2*3",
    "SELECT 7/2",
    "SELECT 7%2",
    "SELECT (1+2)*3",
    "SELECT -3",
    "SELECT 'a'||'b'",
    "SELECT 'n='||1",
    "SELECT 1+NULL",
    "SELECT 1=1",
    "SELECT 1<2",
    "SELECT 2<>2",
    # --- date / time. `22007` (not a date) and `22008` (a date that cannot
    # exist) are DIFFERENT codes, and PostgreSQL canonicalises the spelling.
    # --- numeric. SCALE is part of the value: `1.50` is not `1.5`.
    "SELECT 1.5",
    "SELECT 1.50::numeric::text",
    "SELECT '0.1'::numeric::text",
    "SELECT '-0.30'::numeric::text",
    "SELECT '2.5000000000000000'::numeric::text",
    "SELECT NULL::numeric",
    "SELECT '2026-09-01 12:34:56'::timestamp::text",
    "SELECT '2026-09-01 12:34:56.789'::timestamp::text",
    "SELECT '2026-09-01 12:34:56.789012'::timestamp::text",
    "SELECT '2026-09-01 12:34:56.000'::timestamp::text",
    "SELECT '2026-09-01'::timestamp::text",
    "SELECT '2026-09-01T12:34:56'::timestamp::text",
    "SELECT NULL::timestamp",
    "SELECT '2026-09-01'::date::text",
    "SELECT '2026-9-1'::date::text",
    "SELECT '20260901'::date::text",
    "SELECT '12:34:56'::time::text",
    "SELECT '12:34'::time::text",
    "SELECT '12:34:56.5'::time::text",
    "SELECT '12:34:56.000'::time::text",
    "SELECT NULL::date",
    "SELECT NULL::time",
    "SELECT '1'::int",
    "SELECT 1::text",
    "SELECT '1.5'::float8",
    "SELECT 'true'::bool",
    "SELECT NULL::int",
    "SELECT '42'::int8",
    "SELECT 1::float8",
    "SELECT id FROM d WHERE n = '3'::int",
    "SELECT id FROM d WHERE n > '1'::int",
    "SELECT * FROM d ORDER BY id",
    "SELECT n, id FROM d ORDER BY id",
    "SELECT id, id FROM d ORDER BY id",
    # --- more three-valued logic, hunting for the next `<>`-shaped bug -------
    "SELECT id FROM d WHERE n >= 1",
    "SELECT id FROM d WHERE n <= 2",
    "SELECT id FROM d WHERE n < 3",
    "SELECT id FROM d WHERE s <> 'a'",
    "SELECT id FROM d WHERE s > 'a'",
    "SELECT id FROM d WHERE n <> 1 OR s IS NULL",
    "SELECT id FROM d WHERE NOT (n IS NULL)",
    "SELECT id FROM d WHERE NOT (n IS NOT NULL)",
    "SELECT id FROM d WHERE NOT (n = 1)",
    "SELECT id FROM d WHERE NOT (n <> 1)",
    "SELECT id FROM d WHERE NOT (n > 1)",
    "SELECT id FROM d WHERE NOT (n <= 2)",
    "SELECT id FROM d WHERE NOT (n = 1 AND s IS NULL)",
    "SELECT id FROM d WHERE NOT (n = 1 OR n = 2)",
    "SELECT id FROM d WHERE NOT (NOT (n = 1))",
    "SELECT id FROM d WHERE NOT (n IN (1, 3))",
    "SELECT id FROM d WHERE NOT (n NOT IN (1, 3))",
    "SELECT id FROM d WHERE NOT (n BETWEEN 1 AND 2)",
    "SELECT id FROM d WHERE NOT (n NOT BETWEEN 1 AND 2)",
    "SELECT id FROM d WHERE NOT (s IS NULL) AND n > 1",
    "SELECT id FROM d WHERE n = 1 AND s IS NULL",
    "SELECT id FROM d WHERE (n = 1 OR n = 2) AND id <> 3",
    "SELECT id FROM d WHERE n IN (1)",
    "SELECT id FROM d WHERE n NOT IN (1, 2)",
    "SELECT id FROM d WHERE s NOT IN ('a')",
    "SELECT id FROM d WHERE n BETWEEN 2 AND 1",
    "SELECT id FROM d WHERE n NOT BETWEEN 2 AND 3",
    "SELECT id FROM d WHERE id BETWEEN 1 AND 4",
    # --- ordering interactions ----------------------------------------------
    "SELECT id FROM d WHERE n IS NOT NULL ORDER BY n DESC",
    "SELECT id FROM d ORDER BY s NULLS FIRST",
    "SELECT id FROM d ORDER BY n ASC, s DESC",
    "SELECT id FROM d ORDER BY id LIMIT 10 OFFSET 0",
    # --- aggregates: NULL handling is the whole game ------------------------
    "SELECT count(*) FROM d",
    "SELECT count(n) FROM d",
    "SELECT count(s) FROM d",
    "SELECT sum(n) FROM d",
    "SELECT min(n) FROM d",
    "SELECT max(n) FROM d",
    "SELECT min(s) FROM d",
    "SELECT max(s) FROM d",
    "SELECT count(*), count(n), sum(n), min(n), max(n) FROM d",
    # Over an empty input: count is 0, everything else is NULL.
    "SELECT count(*) FROM d WHERE n > 99",
    "SELECT count(n) FROM d WHERE n > 99",
    "SELECT sum(n) FROM d WHERE n > 99",
    "SELECT min(n) FROM d WHERE n > 99",
    "SELECT max(n) FROM d WHERE n > 99",
    "SELECT count(*) FROM d WHERE n IS NOT NULL",
    "SELECT sum(n) FROM d WHERE n <> 1",
    # --- GROUP BY: NULL is its own group ------------------------------------
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s",
    "SELECT s, count(n) FROM d GROUP BY s ORDER BY s",
    "SELECT s, sum(n) FROM d GROUP BY s ORDER BY s",
    "SELECT s, min(n), max(n) FROM d GROUP BY s ORDER BY s",
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s DESC",
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s NULLS FIRST",
    "SELECT s, count(*) FROM d WHERE n IS NOT NULL GROUP BY s ORDER BY s",
    "SELECT s AS grp, count(*) AS c FROM d GROUP BY s ORDER BY s",
    "SELECT s, count(*) FROM d GROUP BY s ORDER BY s LIMIT 2",
    "SELECT count(*) FROM d GROUP BY s ORDER BY s",
    # --- arrays: text form, comparison, and NULL's array-only rule ----------
    # Array `=` is NOT scalar `=` applied elementwise: inside an array two
    # NULLs compare EQUAL and a NULL sorts after every non-NULL. Scalar
    # `NULL = NULL` is NULL, so this is the case that catches a compare
    # written by analogy with the scalar path.
    "SELECT ARRAY[1,2,3]::int[]",
    "SELECT ARRAY['a','b']::text[]",
    "SELECT '{}'::text[]",
    "SELECT '{1,2,3}'::int[]",
    "SELECT '{foo,\"bar baz\",qux}'::text[]",
    "SELECT ARRAY[NULL]::text[] = ARRAY[NULL]::text[]",
    "SELECT ARRAY['a',NULL]::text[] = ARRAY['a',NULL]::text[]",
    "SELECT ARRAY['a',NULL]::text[] > ARRAY['a','z']::text[]",
    "SELECT ARRAY['a']::text[] < ARRAY['a','b']::text[]",
    "SELECT ARRAY[1,2]::int[] = ARRAY[1,2]::int[]",
    "SELECT ARRAY[1,2]::int[] = ARRAY[1,3]::int[]",
    "SELECT ARRAY[1,2]::int[] < ARRAY[1,3]::int[]",
    "SELECT ARRAY[2]::int[] > ARRAY[1,9,9]::int[]",
    "SELECT '{}'::int[] = '{}'::int[]",
    "SELECT '{}'::int[] < ARRAY[1]::int[]",
    "SELECT ARRAY[1,2,3]::int[] <> ARRAY[1,2]::int[]",
    "SELECT '{a,NULL,b}'::text[]",
    # --- pg_typeof: the DISPLAY name, which is not the internal one ---------
    "SELECT pg_typeof(1)::text",
    "SELECT pg_typeof(1::int8)::text",
    "SELECT pg_typeof(1::int2)::text",
    "SELECT pg_typeof(1.5)::text",
    "SELECT pg_typeof(1.5::float8)::text",
    "SELECT pg_typeof(1.5::float4)::text",
    "SELECT pg_typeof('a'::text)::text",
    "SELECT pg_typeof('a'::varchar)::text",
    "SELECT pg_typeof('a'::bpchar)::text",
    "SELECT pg_typeof('x'::name)::text",
    "SELECT pg_typeof(true)::text",
    "SELECT pg_typeof('2026-01-01'::date)::text",
    "SELECT pg_typeof('12:00'::time)::text",
    "SELECT pg_typeof('2026-01-01 12:00'::timestamp)::text",
    "SELECT pg_typeof(ARRAY[1,2])::text",
    "SELECT pg_typeof(ARRAY['a']::text[])::text",
    # A bare NULL has no type yet — PostgreSQL calls it `unknown`.
    "SELECT pg_typeof(null)::text",
    "SELECT pg_typeof(1+1)::text",
    "SELECT pg_typeof('a'||'b')::text",
    "SELECT pg_typeof(1=1)::text",
    # --- casts to integer use TWO different rounding rules ------------------
    # numeric -> integer rounds half AWAY FROM ZERO; float -> integer rounds
    # half TO EVEN. Using one rule for both answers 3 for `2.5::float8::int`.
    "SELECT 1.5::int, 2.5::int, -1.5::int, 0.5::int, 1.4::int, -0.5::int",
    "SELECT 0.5::float8::int, 1.5::float8::int, 2.5::float8::int",
    "SELECT 3.5::float8::int, -0.5::float8::int, -2.5::float8::int",
    "SELECT 1.5::int8, 1.5::int2",
    # A decimal literal is `numeric`, so these cast FROM numeric, not float.
    "SELECT 1.5::float8, 1.5::float4",
    # --- a timestamp CONSTANT reaches the wire by a different route than a
    # stored one, and used to come back NULL --------------------------------
    "SELECT '2026-01-01 12:00'::timestamp",
    "SELECT '2026-01-01 12:00:00.123456'::timestamp",
    "SELECT '1969-07-20 20:17:40'::timestamp",
    "SELECT '2026-01-01 12:00'::timestamp::text",
    # --- interval: three independent parts, flattened only for comparison ---
    "SELECT '1 day'::interval::text",
    "SELECT '1 day 02:03:04'::interval::text",
    "SELECT '1d 3h 4m 5.678s'::interval::text",
    "SELECT '1 year 2 months'::interval::text",
    "SELECT 'P1Y2M3D'::interval::text",
    "SELECT 'PT1H2M3S'::interval::text",
    "SELECT '1 mon -1 day'::interval::text",
    "SELECT '1.5 days'::interval::text",
    "SELECT '1 week'::interval::text",
    "SELECT '12 mons'::interval::text",
    "SELECT '13 mons'::interval::text",
    "SELECT '0'::interval::text",
    "SELECT '25:00:00'::interval::text",
    "SELECT '0.5 sec'::interval::text",
    "SELECT '500 ms'::interval::text",
    "SELECT '1000 us'::interval::text",
    "SELECT '2 hrs 30 mins'::interval::text",
    "SELECT '-1 day'::interval::text",
    "SELECT '-1 mon'::interval::text",
    "SELECT '-13 mons'::interval::text",
    "SELECT '-1.5 hours'::interval::text",
    "SELECT '1 day -02:03:04'::interval::text",
    "SELECT '100 years'::interval::text",
    "SELECT pg_typeof('1 day'::interval)::text",
    # Comparison flattens: 30-day months, 24-hour days.
    "SELECT '1 day'::interval = '24:00:00'::interval",
    "SELECT '1 mon'::interval = '30 days'::interval",
    "SELECT '1 day'::interval < '25:00:00'::interval",
    "SELECT '1 day'::interval > '1 hour'::interval",
    # Arithmetic keeps them apart: months clamp to the month end.
    "SELECT ('2026-01-31'::timestamp + '1 mon'::interval)::text",
    "SELECT ('2026-01-31'::timestamp + '2 mons'::interval)::text",
    "SELECT ('2024-02-29'::timestamp + '1 year'::interval)::text",
    "SELECT ('2026-03-01 12:00'::timestamp - '1 day'::interval)::text",
    "SELECT ('2026-01-01 00:00'::timestamp + '1d 3h 4m 5.678s'::interval)::text",
    "SELECT ('1 day'::interval + '2 hours'::interval)::text",
    "SELECT ('1 mon'::interval - '1 day'::interval)::text",
    # Beside an interval, a bare UNKNOWN literal coerces to an INTERVAL — not
    # to a timestamp — so this is 22007 rather than date arithmetic. A typed
    # operand keeps datetime arithmetic. `tasks/backlog.md` recorded this rule
    # from the Python server; the Rust one reproduced the same bug until it did.
    "SELECT ('1 day' + interval '1 day')::text",
    "SELECT (interval '1 day' - '2 hours')::text",
    "SELECT ('2020-01-01'::timestamp + interval '1 day')::text",
    "SELECT ('2020-01-01'::date + interval '1 day')::text",
    # Scaling spills fractions downward: months to days, days to time.
    "SELECT (interval '1 day' * 2)::text",
    "SELECT (interval '1 day' * 0.5)::text",
    "SELECT (interval '1 mon' * 1.5)::text",
    "SELECT (interval '1 year' * 0.5)::text",
    "SELECT (interval '1 day' / 2)::text",
    "SELECT (interval '1 mon 1 day' * 2)::text",
    "SELECT (2 * interval '1 day')::text",
    # --- numeric arithmetic is EXACT and its scale is part of the answer ----
    "SELECT (1.5 + 1.5)::text",
    "SELECT (1.50 + 1.5)::text",
    "SELECT (1 + 1.5)::text",
    "SELECT (1.234 + 1.1)::text",
    "SELECT (2.00 - 1.0)::text",
    "SELECT (2.5 * 2)::text",
    "SELECT (2.5 * 2.0)::text",
    "SELECT (1.50 * 1.50)::text",
    "SELECT (0.1 * 0.1)::text",
    "SELECT (0.1 + 0.2)::text",
    "SELECT (-1.5)::text",
    "SELECT (2 * 3)::text",
    # --- NaN has a place in PostgreSQL's TOTAL order, unlike IEEE's ---------
    "SELECT 'NaN'::float8 = 'NaN'::float8",
    "SELECT 'NaN'::float8 > 1e308",
    "SELECT 'NaN'::float8 > 'Infinity'::float8",
    "SELECT 'Infinity'::float8 > 1e308",
    "SELECT -'Infinity'::float8 < -1e308",
    "SELECT 'NaN'::numeric = 'NaN'::numeric",
    # --- exact decimal comparison: the same f64, different numerics --------
    "SELECT 1.50::numeric = 1.5::numeric",
    "SELECT 0::numeric = -0::numeric",
    "SELECT '12345678901234567890.1'::numeric < '12345678901234567890.2'::numeric",
    "SELECT (-1.5)::numeric < (-1.4)::numeric",
    # --- an unknown literal takes the type of the operand beside it --------
    "SELECT interval '1 day' = '1 day'",
    "SELECT '1 day' = interval '1 day'",
    "SELECT '2026-01-01'::timestamp = '2026-01-01'",
    "SELECT '2026-01-01'::date = '2026-01-01'",
    "SELECT ARRAY[1,2] = '{1,2}'",
    "SELECT ARRAY['a','b'] = '{a,b}'",
    # --- json keeps what it was given; jsonb normalises --------------------
    """SELECT '{"b":1, "a":2}'::json::text""",
    """SELECT '{"b":1, "a":2}'::jsonb::text""",
    """SELECT '{"a":1, "a":2}'::jsonb::text""",
    """SELECT '[1,  2,   3]'::jsonb::text""",
    # Keys sort by BYTE length first, then bytewise.
    """SELECT '{"aa":1,"ab":2,"b":3}'::jsonb::text""",
    """SELECT '{"é":1,"z":2}'::jsonb::text""",
    """SELECT '{"b":1,"A":2}'::jsonb::text""",
    """SELECT '{"nested": {"z":1,"a":2}}'::jsonb::text""",
    # A jsonb number is a numeric: the exponent expands, the scale survives.
    """SELECT '{"x": 1.10}'::jsonb::text""",
    """SELECT '{"n":-1.5e10}'::jsonb::text""",
    """SELECT '{"n":1e3}'::jsonb::text""",
    """SELECT '{"n":1.5E-3}'::jsonb::text""",
    """SELECT '{"big":123456789012345678901234567890}'::jsonb::text""",
    "SELECT '\"str\"'::jsonb::text",
    "SELECT 'null'::jsonb::text",
    "SELECT '[]'::jsonb::text",
    "SELECT '{}'::jsonb::text",
    "SELECT pg_typeof('{}'::json)::text",
    "SELECT pg_typeof('{}'::jsonb)::text",
    # --- scalar built-ins. Bare, NOT wrapped in a cast: only the cast route
    # goes through the expression evaluator, so a probe that always casts
    # never exercises the target-list path (which is how it stayed broken).
    "SELECT upper('aB')",
    "SELECT initcap('ab cd')",
    "SELECT length('héllo')",
    "SELECT octet_length('héllo')",
    "SELECT btrim('xxaxx','x')",
    "SELECT substr('abcdef',2,3)",
    "SELECT replace('abcabc','b','X')",
    "SELECT repeat('ab',3)",
    "SELECT reverse('abc')",
    "SELECT left('abcde',-2)",
    "SELECT right('abcde',-2)",
    "SELECT strpos('abcabc','c')",
    "SELECT strpos('abc','z')",
    "SELECT concat('a',null,'b')",
    "SELECT concat_ws('-','a',null,'b')",
    "SELECT split_part('a,b,c',',',2)",
    "SELECT md5('a')",
    "SELECT chr(233)",
    "SELECT ascii('é')",
    "SELECT starts_with('abc','ab')",
    "SELECT abs(-5.5)",
    "SELECT sign(-3)",
    "SELECT ceil(-1.2)",
    "SELECT floor(-1.7)",
    "SELECT trunc(-1.9)",
    "SELECT trunc(1.999,2)",
    "SELECT round(1.234,2)",
    # numeric rounds half away from zero, float8 half to even.
    "SELECT round(1.5)",
    "SELECT round(2.5)",
    "SELECT round(-1.5)",
    "SELECT round(1.5::float8)",
    "SELECT round(2.5::float8)",
    "SELECT power(2,3)",
    "SELECT exp(1)",
    "SELECT ln(1)",
    "SELECT log(100)",
    "SELECT sqrt(4)",
    "SELECT mod(-7,3)",
    "SELECT div(7,3)",
    # greatest / least IGNORE nulls; almost everything else propagates them.
    "SELECT greatest(1,2,3)",
    "SELECT least(3,2,1)",
    "SELECT greatest(1,null)",
    "SELECT greatest(null,null)",
    "SELECT coalesce(null,1)",
    "SELECT coalesce(null,null)",
    "SELECT nullif(1,1)",
    "SELECT nullif(1,2)",
    "SELECT upper(null)",
    # --- ranges: discrete types canonicalise to [), continuous ones do not --
    "SELECT int4range(1,5)::text",
    "SELECT int4range(1,5,'[]')::text",
    "SELECT int4range(1,5,'()')::text",
    "SELECT '[1,5)'::int4range::text",
    "SELECT '[1,5]'::int4range::text",
    "SELECT '(1,5)'::int4range::text",
    "SELECT '(1,5]'::int4range::text",
    "SELECT 'empty'::int4range::text",
    "SELECT int4range(1,1)::text",
    "SELECT '[1,1)'::int4range::text",
    "SELECT int4range(null,5)::text",
    "SELECT int4range(1,null)::text",
    "SELECT '(,)'::int4range::text",
    "SELECT int8range(1,5)::text",
    "SELECT daterange('2026-01-01','2026-01-05')::text",
    "SELECT '[2026-01-01,2026-01-05]'::daterange::text",
    # numrange is continuous — the bounds stay exactly as written.
    "SELECT numrange(1.0,2.0)::text",
    "SELECT numrange(1,2,'[]')::text",
    "SELECT '[1.0,2.0]'::numrange::text",
    "SELECT '[1.0,2.00]'::numrange::text",
    # A timestamp bound is quoted, because it has a space in it.
    "SELECT tsrange('2026-01-01','2026-01-02')::text",
    # Two spellings of one range are equal.
    "SELECT '[1,5]'::int4range = '[1,6)'::int4range",
    "SELECT '[1,5)'::int4range = '[1,5)'::int4range",
    "SELECT pg_typeof(int4range(1,5))::text",
    "SELECT pg_typeof('[1,2)'::numrange)::text",
    # A tsrange orders its own bounds, so a sub-millisecond bound has to be
    # comparable — two timestamp composites had no comparison arm.
    "SELECT tsrange('2026-01-01 00:00:00.5','2026-01-02')::text",
    "SELECT '[2026-01-01 00:00:00.5,2026-01-02)'::tsrange::text",
    # --- multiranges: sorted, empties dropped, touching members merged ------
    "SELECT '{[1,5)}'::int4multirange::text",
    "SELECT '{[10,20),[1,5)}'::int4multirange::text",
    "SELECT '{[1,5),[3,8)}'::int4multirange::text",
    # touching merges; a gap does not
    "SELECT '{[1,5),[5,8)}'::int4multirange::text",
    "SELECT '{[1,5),[6,8)}'::int4multirange::text",
    "SELECT '{[1,2),[2,3),[3,4)}'::int4multirange::text",
    "SELECT '{[1,5),[2,3)}'::int4multirange::text",
    "SELECT '{}'::int4multirange::text",
    "SELECT '{empty}'::int4multirange::text",
    "SELECT '{[1,5),empty,[10,20)}'::int4multirange::text",
    "SELECT '{[1,5]}'::int4multirange::text",
    "SELECT '{(,5)}'::int4multirange::text",
    "SELECT '{(,5),[10,)}'::int4multirange::text",
    # a continuous element type has no adjacency by stepping
    "SELECT '{[1.0,2.0),[2.0,3.0)}'::nummultirange::text",
    "SELECT '{[1.0,2.0),(2.0,3.0)}'::nummultirange::text",
    "SELECT '{[2026-01-01,2026-01-05)}'::datemultirange::text",
    "SELECT int4multirange()::text",
    "SELECT int4multirange(int4range(1,5))::text",
    "SELECT int4multirange(int4range(1,5),int4range(10,20))::text",
    "SELECT int8multirange(int8range(1,5))::text",
    "SELECT pg_typeof('{}'::int4multirange)::text",
    "SELECT '{[1,5)}'::int4multirange = '{[1,5)}'::int4multirange",
    # --- generate_series as a FROM source ----------------------------------
    "SELECT * FROM generate_series(1,5)",
    "SELECT * FROM generate_series(1,10,3)",
    # counting up towards a smaller stop is EMPTY, not reversed
    "SELECT * FROM generate_series(5,1)",
    "SELECT * FROM generate_series(5,1,-2)",
    "SELECT * FROM generate_series(1,0)",
    "SELECT * FROM generate_series(3,3)",
    # the alias renames the column; a column alias beats the table one
    "SELECT * FROM generate_series(1,3) AS g",
    "SELECT * FROM generate_series(1,3) AS g(x)",
    "SELECT g FROM generate_series(1,3) g",
    "SELECT * FROM generate_series(1,3) ORDER BY 1 DESC",
    "SELECT * FROM generate_series(1,10) LIMIT 3",
    "SELECT * FROM generate_series(1,10) OFFSET 7",
    "SELECT * FROM generate_series(1,10) LIMIT 2 OFFSET 3",
    "SELECT count(*) FROM generate_series(1,100)",
    "SELECT sum(g) FROM generate_series(1,10) g",
    "SELECT min(g), max(g) FROM generate_series(3,7) g",
    # --- the same function in the SELECT LIST, with no FROM at all ---------
    "SELECT generate_series(1,3)",
    "SELECT generate_series(1,3) AS g",
    "SELECT generate_series(3,1)",
    "SELECT generate_series(1,5,2)",
    "SELECT generate_series(1,3) ORDER BY 1 DESC",
    "SELECT generate_series(1,10) LIMIT 3",
    "SELECT generate_series(1,10) OFFSET 7",
]

# (statement, verification query) — the write is compared by its row count AND
# by what the table looks like afterwards.
MUTATIONS = [
    ("UPDATE d SET n = 99 WHERE id = 1", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 5 WHERE n IS NULL", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET s = 'z' WHERE n > 1", "SELECT id, s FROM d ORDER BY id"),
    ("UPDATE d SET n = 7 WHERE id = 999", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 1", "SELECT id, n FROM d ORDER BY id"),
    ("DELETE FROM d WHERE id = 2", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n IS NULL", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n > 1", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE id = 999", "SELECT id FROM d ORDER BY id"),
    ("UPDATE d SET n = NULL WHERE id = 1", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET s = NULL", "SELECT id, s FROM d ORDER BY id"),
    ("UPDATE d SET n = 4 WHERE n IN (1, 2)", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 0 WHERE n <> 3", "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET n = 8 WHERE n BETWEEN 1 AND 2", "SELECT id, n FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n IS NOT NULL", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n IN (1, 3)", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n <> 1", "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d", "SELECT id FROM d ORDER BY id"),
]


@pytest.fixture(scope="module")
def oracle() -> Iterator[psycopg.Connection]:
    """The live PostgreSQL, isolated to THIS xdist worker's own schema.

    Our side gets a fresh storage home per test, but there is only one local
    PostgreSQL and every case here creates a table called `d`. Sharing the
    `public` schema across xdist workers made them race on `CREATE TABLE d`
    (`duplicate key ... pg_type_typname_nsp_index`) — 35 failures under `-n
    auto` that every one of them passed serially. A per-worker schema removes
    the shared name entirely.
    """
    conn = _oracle()
    if conn is None:
        # Reachable at import but not now: report it as a skip, matching the
        # other oracle-backed suites. An ERROR here reads as a server bug when
        # it is the oracle that went away.
        pytest.skip(f"PostgreSQL oracle unreachable ({ORACLE_DSN}): {_LAST_ERROR[0]}")
    worker = os.environ.get("PYTEST_XDIST_WORKER", "serial")
    schema = f"secantus_diff_{worker}"
    try:
        cur = conn.cursor()
        # Recreate rather than reuse: a previous run's leftovers would seed the
        # oracle with rows this run never inserted.
        cur.execute(f"DROP SCHEMA IF EXISTS {schema} CASCADE")
        cur.execute(f"CREATE SCHEMA {schema}")
        cur.execute(f"SET search_path TO {schema}")
        yield conn
        cur.execute(f"DROP SCHEMA IF EXISTS {schema} CASCADE")
    finally:
        conn.close()


def _reset_oracle(conn: psycopg.Connection) -> None:
    cur = conn.cursor()
    cur.execute("DROP TABLE IF EXISTS d")
    for sql in SETUP:
        cur.execute(sql)


def _rows(cur: psycopg.Cursor, sql: str) -> list[tuple]:
    cur.execute(sql)
    return cur.fetchall()


@pytest.fixture
def ours(tmp_path: Path) -> Iterator[psycopg.Connection]:
    """A freshly seeded secantusd-pg."""
    home = tmp_path / "pgstore"
    home.mkdir()
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql in SETUP:
            cur.execute(sql)
        yield conn


@pytest.mark.parametrize("sql", QUERIES, ids=lambda s: s[:58])
def test_query_matches_postgres(
    sql: str, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    _reset_oracle(oracle)
    theirs = _rows(oracle.cursor(), sql)
    mine = _rows(ours.cursor(), sql)
    assert mine == theirs, f"{sql}\n  postgres={theirs}\n  ours    ={mine}"


# (sql, params). These go over the EXTENDED protocol -- Parse/Bind/Execute --
# because psycopg switches to it as soon as a query has parameters. Until the
# server implemented that path it answered "OK" with zero rows, which is a wrong
# answer rather than a missing feature, and no literal-SQL case could catch it.
PARAMETERISED = [
    ("SELECT id FROM d WHERE n > %s", (1,)),
    ("SELECT id FROM d WHERE n = %s", (3,)),
    ("SELECT id FROM d WHERE n <> %s", (1,)),
    ("SELECT id FROM d WHERE s = %s", ("a",)),
    ("SELECT id FROM d WHERE s <> %s", ("a",)),
    ("SELECT id FROM d WHERE n >= %s AND n <= %s", (1, 2)),
    ("SELECT id FROM d WHERE n IN (%s, %s)", (1, 3)),
    ("SELECT id FROM d WHERE n NOT IN (%s)", (1,)),
    ("SELECT id FROM d WHERE n BETWEEN %s AND %s", (1, 2)),
    ("SELECT id FROM d WHERE NOT (n = %s)", (1,)),
    ("SELECT id FROM d WHERE s = %s OR n > %s", ("a", 2)),
    ("SELECT id FROM d ORDER BY id LIMIT %s", (2,)),
    ("SELECT id FROM d ORDER BY id LIMIT %s OFFSET %s", (2, 1)),
    ("SELECT id, n FROM d WHERE n IS NOT NULL ORDER BY id", ()),
    ("SELECT count(*) FROM d WHERE n > %s", (1,)),
    ("SELECT sum(n) FROM d WHERE n > %s", (1,)),
    ("SELECT count(*) FROM d WHERE n > %s", (99,)),
    ("SELECT sum(n) FROM d WHERE n > %s", (99,)),
    ("SELECT s, count(*) FROM d GROUP BY s ORDER BY s", ()),
    # Bound values of every type this server decodes, which psycopg sends in
    # BINARY by default — a format decoded by entirely separate code.
    ("SELECT %s::numeric", (Decimal("1.50"),)),
    ("SELECT %s::numeric", (Decimal("-12345.6789"),)),
    ("SELECT %s::date", (dt.date(2026, 9, 2),)),
    ("SELECT %s::time", (dt.time(23, 59, 59, 123456),)),
    ("SELECT %s::timestamp", (dt.datetime(2026, 9, 2, 12, 34, 56),)),
    ("SELECT %s::timestamp", (dt.datetime(1969, 7, 20, 20, 17, 40),)),
    ("SELECT %s::int4[]", ([1, 2, 3],)),
    ("SELECT %s::int4[]", ([1, None, 3],)),
    ("SELECT %s::text[]", (["a", "b"],)),
    ("SELECT %s::int8[]", ([10**12, 2],)),
    # Multiranges as bound parameters — psycopg sends these in binary, whose
    # layout is a range count then each range in the RANGE's own binary form.
    ("SELECT %s::int4multirange", (Multirange([Range(1, 5, "[)")]),)),
    (
        "SELECT %s::int4multirange",
        (Multirange([Range(1, 5, "[)"), Range(10, 20, "[)")]),),
    ),
    ("SELECT %s::int4multirange", (Multirange([]),)),
    (
        "SELECT %s::nummultirange",
        (Multirange([Range(Decimal("1.0"), Decimal("2.0"), "[]")]),),
    ),
    ("SELECT %s::interval", (dt.timedelta(days=1),)),
    ("SELECT %s::interval", (dt.timedelta(days=1, hours=2, minutes=3, seconds=4),)),
    ("SELECT %s::interval", (dt.timedelta(seconds=-1),)),
    ("SELECT %s::interval", (dt.timedelta(microseconds=500000),)),
    # A typed value compared against a bound one. psycopg leaves the parameter
    # type UNSPECIFIED for lists and datetimes and lets the server infer it, so
    # these exercise the resolution rule rather than a declared oid.
    ("SELECT array['a','b'] = %s", (["a", "b"],)),
    # NOT `array[1,2,3] = %s`: psycopg dumps a list of small ints as
    # `smallint[]`, and PostgreSQL has no `integer[] = smallint[]` operator —
    # array comparison needs identical element types, with no widening. This
    # server compares them and answers true. Divergence filed in the backlog.
    ("SELECT '1 day'::interval = %s", (dt.timedelta(days=1),)),
    ("SELECT '2026-01-01 12:00'::timestamp = %s", (dt.datetime(2026, 1, 1, 12, 0),)),
    ("SELECT '2026-01-01'::date = %s", (dt.date(2026, 1, 1),)),
    ("SELECT '12:00'::time = %s", (dt.time(12, 0),)),
    ("SELECT 1.50::numeric = %s", (Decimal("1.5"),)),
    ("SELECT array[1.5::numeric] = %s", ([Decimal("1.5")],)),
    ("SELECT array['2026-01-01'::date] = %s", ([dt.date(2026, 1, 1)],)),
    # A range constructor whose BOUNDS argument is a parameter. Describe runs
    # before Bind, so at plan time that argument is NULL — which is an error
    # for a literal and must not be one for a placeholder.
    ("SELECT int4range(%s::int4, %s::int4, %s)", (10, 20, "[]")),
    ("SELECT int4range(%s::int4, %s::int4, %s)", (None, None, "()")),
    ("SELECT int4range(%s::int4, %s::int4, %s)", (10, None, "[)")),
    ("SELECT numrange(%s::numeric, %s::numeric, %s)", (Decimal("-100"), Decimal("100.123"), "(]")),
    # A bound NULL must behave exactly like a literal one.
    ("SELECT id FROM d WHERE n = %s", (None,)),
    ("SELECT id FROM d WHERE n <> %s", (None,)),
    ("SELECT id FROM d WHERE n IN (%s, %s)", (1, None)),
]

PARAM_MUTATIONS = [
    ("UPDATE d SET n = %s WHERE id = %s", (99, 1), "SELECT id, n FROM d ORDER BY id"),
    ("UPDATE d SET s = %s WHERE n > %s", ("z", 1), "SELECT id, s FROM d ORDER BY id"),
    ("UPDATE d SET n = %s WHERE n IS NULL", (7,), "SELECT id, n FROM d ORDER BY id"),
    ("DELETE FROM d WHERE id = %s", (2,), "SELECT id FROM d ORDER BY id"),
    ("DELETE FROM d WHERE n > %s", (1,), "SELECT id FROM d ORDER BY id"),
    ("INSERT INTO d VALUES (%s, %s, %s)", (9, 9, "i"), "SELECT id, n, s FROM d ORDER BY id"),
]


# (zone, query). `timestamptz` renders in the SESSION zone, so every one of
# these is meaningless without setting it on BOTH servers first — the oracle's
# own default here is `GB`, not UTC, which would make a zone-less comparison
# look like a divergence in this server.
TIMEZONE_QUERIES = [
    (tz, sql)
    for tz in ["UTC", "+02:00", "-02:00", "Europe/Rome", "America/Chicago"]
    for sql in [
        "SELECT '2026-01-01 12:00'::timestamptz::text",
        # July as well as January: a named zone's offset differs across DST,
        # a fixed one does not.
        "SELECT '2026-07-01 12:00'::timestamptz::text",
        "SELECT '2026-01-01 12:00+00'::timestamptz::text",
        "SELECT '2026-01-01 12:00+02'::timestamptz::text",
        "SELECT '2026-01-01 12:00Z'::timestamptz::text",
        # Second-precision offsets are real and appear in psycopg's corpus.
        "SELECT '2000-01-01 00:00+01:02:03'::timestamptz::text",
        "SELECT '0258-1-8 1:12:32.358261+01:02:03'::timestamptz::text",
        "SELECT pg_typeof('2026-01-01'::timestamptz)::text",
        "SELECT pg_typeof('12:00'::timetz)::text",
        "SELECT '12:00+02'::timetz::text",
        "SELECT 'integer'::regtype::text",
        "SELECT 'int4'::regtype::text",
    ]
]


@pytest.mark.parametrize("tz,sql", TIMEZONE_QUERIES, ids=lambda v: str(v)[:44])
def test_timezone_query_matches_postgres(
    tz: str, sql: str, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    """`timestamptz` under an explicit session TimeZone, on both servers.

    `SET TimeZone TO '+02:00'` uses the POSIX sign — positive is WEST of
    Greenwich, so it renders as `-02`. That is the reverse of the sign in a
    literal like `'12:00+02'`, and was probed rather than assumed.
    """
    for conn in (oracle, ours):
        conn.cursor().execute(f"set timezone to '{tz}'")
    theirs = _rows(oracle.cursor(), sql)
    mine = _rows(ours.cursor(), sql)
    assert mine == theirs, f"[{tz}] {sql}\n  postgres={theirs}\n  ours    ={mine}"
    for conn in (oracle, ours):
        conn.cursor().execute("set timezone to 'UTC'")


# Statements both servers must REFUSE, with the same SQLSTATE. A wrong answer
# and a wrong error code are both divergences, and only the first shows up in a
# row comparison — these would pass a `_rows` test by raising on both sides.
ERROR_QUERIES = [
    # Beside an interval a bare unknown literal coerces to an INTERVAL, so this
    # is a bad interval rather than date arithmetic.
    "SELECT ('2020-01-01' + interval '1 day')::text",
    "SELECT (interval '1 day' + '2020-01-01')::text",
    # …and the same rule in a COMPARISON, not just arithmetic.
    "SELECT interval '1 day' = '2020-01-01'",
    "SELECT 1/0",
    "SELECT 'x'::numeric",
    "SELECT 'x'::int",
    "SELECT '2026-13-01'::date",
    "SELECT nosuchcolumn FROM d",
    "SELECT * FROM nosuchtable",
    "SELECT '{bad}'::json",
    "SELECT '[1,]'::json",
    "SELECT '{\"a\":1} x'::json",
    # Three different mistakes, three different SQLSTATE classes.
    "SELECT int4range(5,1)",
    "SELECT '[5,1)'::int4range",
    "SELECT 'x'::int4range",
    "SELECT int4range(1,5,'x')",
    "SELECT int4range(1,5,null)",
    "SELECT '{[1,5)'::int4multirange",
    "SELECT '{x}'::int4multirange",
    "SELECT '[1,5)'::int4multirange",
    "SELECT * FROM generate_series(1,5,0)",
]


@pytest.mark.parametrize("sql", ERROR_QUERIES, ids=lambda s: s[:52])
def test_error_sqlstate_matches_postgres(
    sql: str, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    """Both servers refuse, and name the same SQLSTATE."""
    # Seed the oracle, exactly as the row comparison does. Without this the
    # fixture table is missing THERE and present HERE, so a bad-column case
    # answers 42P01 against 42703 and looks like a server divergence.
    _reset_oracle(oracle)

    def refusal(conn: psycopg.Connection) -> str:
        try:
            conn.cursor().execute(sql)
        except psycopg.Error as exc:
            return exc.diag.sqlstate or "?"
        else:
            return "accepted"
        finally:
            with contextlib.suppress(Exception):
                conn.rollback()

    theirs = refusal(oracle)
    mine = refusal(ours)
    # This guard has already earned its keep: two cases drafted for this list
    # were ones PostgreSQL ACCEPTS and this server deliberately refuses
    # (nested arrays, a 35-digit numeric). Those are documented limitations,
    # not shared refusals, and belong in the backlog rather than here.
    assert theirs != "accepted", f"the oracle accepted {sql}; the case is wrong"
    assert mine == theirs, f"{sql}\n  postgres={theirs}\n  ours    ={mine}"


@pytest.mark.parametrize("sql,params", PARAMETERISED, ids=lambda v: str(v)[:52])
def test_parameterised_query_matches_postgres(
    sql: str, params: tuple, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    _reset_oracle(oracle)
    ocur, mcur = oracle.cursor(), ours.cursor()
    ocur.execute(sql, params or None)
    theirs = ocur.fetchall()
    mcur.execute(sql, params or None)
    mine = mcur.fetchall()
    assert mine == theirs, f"{sql} {params}\n  postgres={theirs}\n  ours    ={mine}"


@pytest.mark.parametrize("stmt,params,verify", PARAM_MUTATIONS, ids=lambda v: str(v)[:52])
def test_parameterised_mutation_matches_postgres(
    stmt: str,
    params: tuple,
    verify: str,
    ours: psycopg.Connection,
    oracle: psycopg.Connection,
) -> None:
    _reset_oracle(oracle)
    ocur, mcur = oracle.cursor(), ours.cursor()
    ocur.execute(stmt, params or None)
    mcur.execute(stmt, params or None)
    assert mcur.rowcount == ocur.rowcount, (
        f"{stmt} {params}\n  postgres rowcount={ocur.rowcount}\n  ours     rowcount={mcur.rowcount}"
    )
    assert _rows(mcur, verify) == _rows(ocur, verify), f"after {stmt} {params}"


@pytest.mark.parametrize("stmt,verify", MUTATIONS, ids=lambda s: str(s)[:58])
def test_mutation_matches_postgres(
    stmt: str, verify: str, ours: psycopg.Connection, oracle: psycopg.Connection
) -> None:
    _reset_oracle(oracle)
    ocur, mcur = oracle.cursor(), ours.cursor()

    ocur.execute(stmt)
    mcur.execute(stmt)
    # The row count is part of the contract: PostgreSQL's UPDATE tag counts rows
    # MATCHED, so `SET n = 1` reports every row even where the value is unchanged.
    assert mcur.rowcount == ocur.rowcount, (
        f"{stmt}\n  postgres rowcount={ocur.rowcount}\n  ours     rowcount={mcur.rowcount}"
    )
    assert _rows(mcur, verify) == _rows(ocur, verify), f"after {stmt}"
