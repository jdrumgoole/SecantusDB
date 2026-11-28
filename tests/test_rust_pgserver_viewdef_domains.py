"""`pg_get_viewdef` relabels a user domain operand to its base type, as
PostgreSQL does (RP-1095).

An operator or a function over a domain resolves on the domain's base type
(`binary_oper_exact`, `func_select_candidate`, `select_common_type` reduce
it with `getBaseType`), and `coerce_type` relabels the operand to that
base, a RelabelType ruleutils shows: `WHERE ((u.dd)::integer > 2)`. A
domain is kept where nothing coerces it -- the select list, ORDER BY,
`IS NULL`, `count(u.dd)`, a polymorphic or `"any"` argument
(`array_agg(u.dd)`, `to_jsonb(u.dd)`), a CASE / COALESCE whose arguments
are all that domain -- and a SQL-syntax call (`SUBSTRING(u.t FROM 2)`)
prints its arguments with `showimplicit = false`, so hides the relabel.

Each answer is PostgreSQL 15.20's, measured 2026-11-28 on a private,
freshly initdb'd cluster with `_SETUP`, in both print modes (the pretty
form with its newlines read as spaces).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.test_rust_pgserver_slice import BINARY, _Server

pytestmark = pytest.mark.skipif(not BINARY.exists(), reason="secantusd-pg not built")

_SETUP = [
    "create domain dint as int",
    "create domain dint2 as dint",
    "create domain dtxt as text",
    "create domain dvc as varchar(10)",
    "create table u (k int, dd dint, t dtxt, vc dvc, dd2 dint2)",
]

#: Each view's query, and PostgreSQL 15.20's `pg_get_viewdef(v)` and
#: `pg_get_viewdef(v, true)`.
VIEWS: list[tuple[str, str, str]] = [
    (
        "select k from u where dd > 2",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer > 2);",
        " SELECT u.k    FROM u   WHERE u.dd::integer > 2;",
    ),
    (
        "select k from u where dd = k",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer = u.k);",
        " SELECT u.k    FROM u   WHERE u.dd::integer = u.k;",
    ),
    (
        "select dd + 1 as x from u",
        " SELECT ((u.dd)::integer + 1) AS x\n   FROM u;",
        " SELECT u.dd::integer + 1 AS x    FROM u;",
    ),
    (
        "select abs(dd) as x from u",
        " SELECT abs((u.dd)::integer) AS x\n   FROM u;",
        " SELECT abs(u.dd::integer) AS x    FROM u;",
    ),
    (
        "select dd::text as x from u",
        " SELECT (u.dd)::text AS x\n   FROM u;",
        " SELECT u.dd::text AS x    FROM u;",
    ),
    (
        "select dd::bigint as x from u",
        " SELECT (u.dd)::bigint AS x\n   FROM u;",
        " SELECT u.dd::bigint AS x    FROM u;",
    ),
    (
        "select k from u where dd in (1,2)",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer = ANY (ARRAY[1, 2]));",
        " SELECT u.k    FROM u   WHERE u.dd::integer = ANY (ARRAY[1, 2]);",
    ),
    (
        "select k from u where dd is null",
        " SELECT u.k\n   FROM u\n  WHERE (u.dd IS NULL);",
        " SELECT u.k    FROM u   WHERE u.dd IS NULL;",
    ),
    (
        "select dd from u order by dd",
        " SELECT u.dd\n   FROM u\n  ORDER BY u.dd;",
        " SELECT u.dd    FROM u   ORDER BY u.dd;",
    ),
    (
        "select coalesce(dd, 0) as x from u",
        " SELECT COALESCE((u.dd)::integer, 0) AS x\n   FROM u;",
        " SELECT COALESCE(u.dd::integer, 0) AS x    FROM u;",
    ),
    (
        "select case when k > 0 then dd else 0 end as x from u",
        (
            " SELECT\n"
            "        CASE\n"
            "            WHEN (u.k > 0) THEN (u.dd)::integer\n"
            "            ELSE 0\n"
            "        END AS x\n"
            "   FROM u;"
        ),
        (
            " SELECT         CASE             WHEN u.k > 0 THEN"
            " u.dd::integer             ELSE 0         END AS x    FROM"
            " u;"
        ),
    ),
    (
        "select dd from u union select k from u",
        " SELECT u.dd\n   FROM u\nUNION\n SELECT u.k AS dd\n   FROM u;",
        " SELECT u.dd    FROM u UNION  SELECT u.k AS dd    FROM u;",
    ),
    (
        "select k from u where dd between 1 and 3",
        " SELECT u.k\n   FROM u\n  WHERE (((u.dd)::integer >= 1) AND ((u.dd)::integer <= 3));",
        " SELECT u.k    FROM u   WHERE u.dd::integer >= 1 AND u.dd::integer <= 3;",
    ),
    (
        "select k from u where dd = any (array[1,2])",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer = ANY (ARRAY[1, 2]));",
        " SELECT u.k    FROM u   WHERE u.dd::integer = ANY (ARRAY[1, 2]);",
    ),
    (
        "select greatest(dd, k) as x from u",
        " SELECT GREATEST((u.dd)::integer, u.k) AS x\n   FROM u;",
        " SELECT GREATEST(u.dd::integer, u.k) AS x    FROM u;",
    ),
    (
        "select sum(dd) as x from u",
        " SELECT sum((u.dd)::integer) AS x\n   FROM u;",
        " SELECT sum(u.dd::integer) AS x    FROM u;",
    ),
    (
        "select count(dd) as x from u",
        " SELECT count(u.dd) AS x\n   FROM u;",
        " SELECT count(u.dd) AS x    FROM u;",
    ),
    (
        "select max(dd) as x from u",
        " SELECT max((u.dd)::integer) AS x\n   FROM u;",
        " SELECT max(u.dd::integer) AS x    FROM u;",
    ),
    (
        "select dd as x from u group by dd",
        " SELECT u.dd AS x\n   FROM u\n  GROUP BY u.dd;",
        " SELECT u.dd AS x    FROM u   GROUP BY u.dd;",
    ),
    (
        "select t from u where t like 'a%'",
        " SELECT u.t\n   FROM u\n  WHERE ((u.t)::text ~~ 'a%'::text);",
        " SELECT u.t    FROM u   WHERE u.t::text ~~ 'a%'::text;",
    ),
    (
        "select t || 'x' as y from u",
        " SELECT ((u.t)::text || 'x'::text) AS y\n   FROM u;",
        " SELECT u.t::text || 'x'::text AS y    FROM u;",
    ),
    (
        "select upper(t) as y from u",
        " SELECT upper((u.t)::text) AS y\n   FROM u;",
        " SELECT upper(u.t::text) AS y    FROM u;",
    ),
    (
        "select length(t) as y from u",
        " SELECT length((u.t)::text) AS y\n   FROM u;",
        " SELECT length(u.t::text) AS y    FROM u;",
    ),
    (
        "select t from u where t = 'a'",
        " SELECT u.t\n   FROM u\n  WHERE ((u.t)::text = 'a'::text);",
        " SELECT u.t    FROM u   WHERE u.t::text = 'a'::text;",
    ),
    (
        "select -dd as x from u",
        " SELECT (- (u.dd)::integer) AS x\n   FROM u;",
        " SELECT - u.dd::integer AS x    FROM u;",
    ),
    (
        "select k from u where dd::int > 2",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer > 2);",
        " SELECT u.k    FROM u   WHERE u.dd::integer > 2;",
    ),
    (
        "select dd::dint as x from u",
        " SELECT u.dd AS x\n   FROM u;",
        " SELECT u.dd AS x    FROM u;",
    ),
    (
        "select k::dint as x from u",
        " SELECT (u.k)::dint AS x\n   FROM u;",
        " SELECT u.k::dint AS x    FROM u;",
    ),
    (
        "select 3::dint as x from u",
        " SELECT (3)::dint AS x\n   FROM u;",
        " SELECT 3::dint AS x    FROM u;",
    ),
    (
        "select k from u where (dd, k) = (1, 2)",
        " SELECT u.k\n   FROM u\n  WHERE (((u.dd)::integer = 1) AND (u.k = 2));",
        " SELECT u.k    FROM u   WHERE u.dd::integer = 1 AND u.k = 2;",
    ),
    (
        "select nullif(dd, 0) as x from u",
        " SELECT NULLIF((u.dd)::integer, 0) AS x\n   FROM u;",
        " SELECT NULLIF(u.dd::integer, 0) AS x    FROM u;",
    ),
    (
        "select k from u where dd is distinct from 2",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer IS DISTINCT FROM 2);",
        " SELECT u.k    FROM u   WHERE u.dd::integer IS DISTINCT FROM 2;",
    ),
    (
        "select dd from u where exists (select 1 from u u2 where u2.dd = u.dd)",
        (
            " SELECT u.dd\n"
            "   FROM u\n"
            "  WHERE (EXISTS ( SELECT 1\n"
            "           FROM u u2\n"
            "          WHERE ((u2.dd)::integer = (u.dd)::integer)));"
        ),
        (
            " SELECT u.dd    FROM u   WHERE (EXISTS ( SELECT 1          "
            "  FROM u u2           WHERE u2.dd::integer ="
            " u.dd::integer));"
        ),
    ),
    (
        "select array_agg(dd) as x from u",
        " SELECT array_agg(u.dd) AS x\n   FROM u;",
        " SELECT array_agg(u.dd) AS x    FROM u;",
    ),
    (
        "select string_agg(t, ',') as x from u",
        " SELECT string_agg((u.t)::text, ','::text) AS x\n   FROM u;",
        " SELECT string_agg(u.t::text, ','::text) AS x    FROM u;",
    ),
    (
        "select k from u where dd = dd",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer = (u.dd)::integer);",
        " SELECT u.k    FROM u   WHERE u.dd::integer = u.dd::integer;",
    ),
    (
        "select k from u where t = t",
        " SELECT u.k\n   FROM u\n  WHERE ((u.t)::text = (u.t)::text);",
        " SELECT u.k    FROM u   WHERE u.t::text = u.t::text;",
    ),
    (
        "select coalesce(dd, dd) as x from u",
        " SELECT COALESCE(u.dd, u.dd) AS x\n   FROM u;",
        " SELECT COALESCE(u.dd, u.dd) AS x    FROM u;",
    ),
    (
        "select dd + 1.5 as x from u",
        " SELECT ((u.dd)::numeric + 1.5) AS x\n   FROM u;",
        " SELECT u.dd::numeric + 1.5 AS x    FROM u;",
    ),
    (
        "select k from u where dd > 1.5",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::numeric > 1.5);",
        " SELECT u.k    FROM u   WHERE u.dd::numeric > 1.5;",
    ),
    (
        "select k from u where vc = 'a'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.vc)::text = 'a'::text);",
        " SELECT u.k    FROM u   WHERE u.vc::text = 'a'::text;",
    ),
    (
        "select vc || 'x' as y from u",
        " SELECT ((u.vc)::text || 'x'::text) AS y\n   FROM u;",
        " SELECT u.vc::text || 'x'::text AS y    FROM u;",
    ),
    (
        "select k from u where vc like 'a%'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.vc)::text ~~ 'a%'::text);",
        " SELECT u.k    FROM u   WHERE u.vc::text ~~ 'a%'::text;",
    ),
    (
        "select k from u where dd in (select k from u)",
        (
            " SELECT u.k\n"
            "   FROM u\n"
            "  WHERE ((u.dd)::integer IN ( SELECT u_1.k\n"
            "           FROM u u_1));"
        ),
        " SELECT u.k    FROM u   WHERE (u.dd::integer IN ( SELECT u_1.k            FROM u u_1));",
    ),
    (
        "select k from u where k in (select dd from u)",
        " SELECT u.k\n   FROM u\n  WHERE (u.k IN ( SELECT u_1.dd\n           FROM u u_1));",
        " SELECT u.k    FROM u   WHERE (u.k IN ( SELECT u_1.dd            FROM u u_1));",
    ),
    (
        "select avg(dd) as x from u",
        " SELECT avg((u.dd)::integer) AS x\n   FROM u;",
        " SELECT avg(u.dd::integer) AS x    FROM u;",
    ),
    (
        "select min(t) as x from u",
        " SELECT min((u.t)::text) AS x\n   FROM u;",
        " SELECT min(u.t::text) AS x    FROM u;",
    ),
    (
        "select k from u where dd not in (1, 2)",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer <> ALL (ARRAY[1, 2]));",
        " SELECT u.k    FROM u   WHERE u.dd::integer <> ALL (ARRAY[1, 2]);",
    ),
    (
        "select k from u where dd2 > 2",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd2)::integer > 2);",
        " SELECT u.k    FROM u   WHERE u.dd2::integer > 2;",
    ),
    (
        "select dd2 + dd as x from u",
        " SELECT ((u.dd2)::integer + (u.dd)::integer) AS x\n   FROM u;",
        " SELECT u.dd2::integer + u.dd::integer AS x    FROM u;",
    ),
    (
        "select k from u where t ~ 'a'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.t)::text ~ 'a'::text);",
        " SELECT u.k    FROM u   WHERE u.t::text ~ 'a'::text;",
    ),
    (
        "select dd * k as x from u",
        " SELECT ((u.dd)::integer * u.k) AS x\n   FROM u;",
        " SELECT u.dd::integer * u.k AS x    FROM u;",
    ),
    (
        "select coalesce(dd, k) as x from u",
        " SELECT COALESCE((u.dd)::integer, u.k) AS x\n   FROM u;",
        " SELECT COALESCE(u.dd::integer, u.k) AS x    FROM u;",
    ),
    (
        "select dd from u where dd > 2 order by dd + 1",
        " SELECT u.dd\n   FROM u\n  WHERE ((u.dd)::integer > 2)\n  ORDER BY ((u.dd)::integer + 1);",
        " SELECT u.dd    FROM u   WHERE u.dd::integer > 2   ORDER BY (u.dd::integer + 1);",
    ),
    (
        "select k from u where dd is not distinct from k",
        " SELECT u.k\n   FROM u\n  WHERE (NOT ((u.dd)::integer IS DISTINCT FROM u.k));",
        " SELECT u.k    FROM u   WHERE NOT u.dd::integer IS DISTINCT FROM u.k;",
    ),
    (
        "select (dd)::text || t as y from u",
        " SELECT ((u.dd)::text || (u.t)::text) AS y\n   FROM u;",
        " SELECT u.dd::text || u.t::text AS y    FROM u;",
    ),
    (
        "select k from u where dd = (select max(k) from u)",
        (
            " SELECT u.k\n"
            "   FROM u\n"
            "  WHERE ((u.dd)::integer = ( SELECT max(u_1.k) AS max\n"
            "           FROM u u_1));"
        ),
        (
            " SELECT u.k    FROM u   WHERE u.dd::integer = (( SELECT"
            " max(u_1.k) AS max            FROM u u_1));"
        ),
    ),
    (
        "select lower(vc) as y from u",
        " SELECT lower((u.vc)::text) AS y\n   FROM u;",
        " SELECT lower(u.vc::text) AS y    FROM u;",
    ),
    (
        "select greatest(dd, dd2) as x from u",
        " SELECT GREATEST((u.dd)::integer, (u.dd2)::integer) AS x\n   FROM u;",
        " SELECT GREATEST(u.dd::integer, u.dd2::integer) AS x    FROM u;",
    ),
    (
        "select '3'::dint as x from u",
        " SELECT (3)::dint AS x\n   FROM u;",
        " SELECT 3::dint AS x    FROM u;",
    ),
    (
        "select 'a'::dtxt as x from u",
        " SELECT ('a'::text)::dtxt AS x\n   FROM u;",
        " SELECT 'a'::text::dtxt AS x    FROM u;",
    ),
    (
        "select 1.5::dint as x from u",
        " SELECT (1.5)::dint AS x\n   FROM u;",
        " SELECT 1.5::dint AS x    FROM u;",
    ),
    (
        "select (dd + 1)::dint as x from u",
        " SELECT (((u.dd)::integer + 1))::dint AS x\n   FROM u;",
        " SELECT (u.dd::integer + 1)::dint AS x    FROM u;",
    ),
    (
        "select dd::dint2 as x from u",
        " SELECT (u.dd)::dint2 AS x\n   FROM u;",
        " SELECT u.dd::dint2 AS x    FROM u;",
    ),
    (
        "select k::dint2 as x from u",
        " SELECT (u.k)::dint2 AS x\n   FROM u;",
        " SELECT u.k::dint2 AS x    FROM u;",
    ),
    (
        "select dd2::dint as x from u",
        " SELECT (u.dd2)::dint AS x\n   FROM u;",
        " SELECT u.dd2::dint AS x    FROM u;",
    ),
    (
        "select t::dtxt as x from u",
        " SELECT u.t AS x\n   FROM u;",
        " SELECT u.t AS x    FROM u;",
    ),
    (
        "select vc::dvc as x from u",
        " SELECT u.vc AS x\n   FROM u;",
        " SELECT u.vc AS x    FROM u;",
    ),
    (
        "select k from u where k::dint > 2",
        " SELECT u.k\n   FROM u\n  WHERE (((u.k)::dint)::integer > 2);",
        " SELECT u.k    FROM u   WHERE u.k::dint::integer > 2;",
    ),
    (
        "select null::dint as x from u",
        " SELECT (NULL::integer)::dint AS x\n   FROM u;",
        " SELECT NULL::integer::dint AS x    FROM u;",
    ),
    (
        "select case when k > 0 then dd end as x from u",
        (
            " SELECT\n"
            "        CASE\n"
            "            WHEN (u.k > 0) THEN (u.dd)::integer\n"
            "            ELSE NULL::integer\n"
            "        END AS x\n"
            "   FROM u;"
        ),
        (
            " SELECT         CASE             WHEN u.k > 0 THEN"
            " u.dd::integer             ELSE NULL::integer         END"
            " AS x    FROM u;"
        ),
    ),
    (
        "select k from u where dd = '3'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer = 3);",
        " SELECT u.k    FROM u   WHERE u.dd::integer = 3;",
    ),
    (
        "select k from u where t = 'a'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.t)::text = 'a'::text);",
        " SELECT u.k    FROM u   WHERE u.t::text = 'a'::text;",
    ),
    (
        "select coalesce(dd, null) as x from u",
        " SELECT COALESCE((u.dd)::integer, NULL::integer) AS x\n   FROM u;",
        " SELECT COALESCE(u.dd::integer, NULL::integer) AS x    FROM u;",
    ),
    (
        "select coalesce(dd2, dd) as x from u",
        " SELECT COALESCE((u.dd2)::integer, (u.dd)::integer) AS x\n   FROM u;",
        " SELECT COALESCE(u.dd2::integer, u.dd::integer) AS x    FROM u;",
    ),
    (
        "select array[dd, k] as x from u",
        " SELECT ARRAY[(u.dd)::integer, u.k] AS x\n   FROM u;",
        " SELECT ARRAY[u.dd::integer, u.k] AS x    FROM u;",
    ),
    (
        "select substr(t, dd) as x from u",
        " SELECT substr((u.t)::text, (u.dd)::integer) AS x\n   FROM u;",
        " SELECT substr(u.t::text, u.dd::integer) AS x    FROM u;",
    ),
    (
        "select array_append(array[1], dd) as x from u",
        " SELECT array_append(ARRAY[1], (u.dd)::integer) AS x\n   FROM u;",
        " SELECT array_append(ARRAY[1], u.dd::integer) AS x    FROM u;",
    ),
    (
        "select jsonb_build_object('a', dd) as x from u",
        " SELECT jsonb_build_object('a', u.dd) AS x\n   FROM u;",
        " SELECT jsonb_build_object('a', u.dd) AS x    FROM u;",
    ),
    (
        "select to_jsonb(dd) as x from u",
        " SELECT to_jsonb(u.dd) AS x\n   FROM u;",
        " SELECT to_jsonb(u.dd) AS x    FROM u;",
    ),
    (
        "select pg_typeof(dd) as x from u",
        " SELECT pg_typeof(u.dd) AS x\n   FROM u;",
        " SELECT pg_typeof(u.dd) AS x    FROM u;",
    ),
    (
        "select lag(dd) over () as x from u",
        " SELECT lag(u.dd) OVER () AS x\n   FROM u;",
        " SELECT lag(u.dd) OVER () AS x    FROM u;",
    ),
    (
        "select format('%s', dd) as x from u",
        " SELECT format('%s'::text, u.dd) AS x\n   FROM u;",
        " SELECT format('%s'::text, u.dd) AS x    FROM u;",
    ),
    (
        "select to_char(dd, '999') as x from u",
        " SELECT to_char((u.dd)::integer, '999'::text) AS x\n   FROM u;",
        " SELECT to_char(u.dd::integer, '999'::text) AS x    FROM u;",
    ),
    (
        "select max(t) as x from u",
        " SELECT max((u.t)::text) AS x\n   FROM u;",
        " SELECT max(u.t::text) AS x    FROM u;",
    ),
    (
        "select array_position(array[1,2], dd) as x from u",
        " SELECT array_position(ARRAY[1, 2], (u.dd)::integer) AS x\n   FROM u;",
        " SELECT array_position(ARRAY[1, 2], u.dd::integer) AS x    FROM u;",
    ),
    (
        "select length(vc) as x from u",
        " SELECT length((u.vc)::text) AS x\n   FROM u;",
        " SELECT length(u.vc::text) AS x    FROM u;",
    ),
    (
        "select substring(t from 2) as x from u",
        " SELECT SUBSTRING(u.t FROM 2) AS x\n   FROM u;",
        " SELECT SUBSTRING(u.t FROM 2) AS x    FROM u;",
    ),
    (
        "select substring(t from dd) as x from u",
        " SELECT SUBSTRING(u.t FROM u.dd) AS x\n   FROM u;",
        " SELECT SUBSTRING(u.t FROM u.dd) AS x    FROM u;",
    ),
    (
        "select trim(t) as x from u",
        " SELECT TRIM(BOTH FROM u.t) AS x\n   FROM u;",
        " SELECT TRIM(BOTH FROM u.t) AS x    FROM u;",
    ),
    (
        "select position('a' in t) as x from u",
        " SELECT POSITION(('a'::text) IN (u.t)) AS x\n   FROM u;",
        " SELECT POSITION(('a'::text) IN (u.t)) AS x    FROM u;",
    ),
    (
        "select k from u where dd in (k, 2)",
        " SELECT u.k\n   FROM u\n  WHERE (((u.dd)::integer = u.k) OR ((u.dd)::integer = 2));",
        " SELECT u.k    FROM u   WHERE u.dd::integer = u.k OR u.dd::integer = 2;",
    ),
    (
        "select row(dd) = row(k) as x from u",
        " SELECT ((u.dd)::integer = u.k) AS x\n   FROM u;",
        " SELECT u.dd::integer = u.k AS x    FROM u;",
    ),
    (
        "select k from u where (dd, k) < (1, 2)",
        " SELECT u.k\n   FROM u\n  WHERE (ROW((u.dd)::integer, u.k) < ROW(1, 2));",
        " SELECT u.k    FROM u   WHERE (ROW(u.dd::integer, u.k) < ROW(1, 2));",
    ),
    (
        "select k from u where t similar to 'a'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.t)::text ~ similar_to_escape('a'::text));",
        " SELECT u.k    FROM u   WHERE u.t::text ~ similar_to_escape('a'::text);",
    ),
    (
        "select k from u where t ilike 'a'",
        " SELECT u.k\n   FROM u\n  WHERE ((u.t)::text ~~* 'a'::text);",
        " SELECT u.k    FROM u   WHERE u.t::text ~~* 'a'::text;",
    ),
    (
        "select k from u where not (dd > 1)",
        " SELECT u.k\n   FROM u\n  WHERE (NOT ((u.dd)::integer > 1));",
        " SELECT u.k    FROM u   WHERE NOT u.dd::integer > 1;",
    ),
    (
        "select nullif(dd, dd) as x from u",
        " SELECT NULLIF((u.dd)::integer, (u.dd)::integer) AS x\n   FROM u;",
        " SELECT NULLIF(u.dd::integer, u.dd::integer) AS x    FROM u;",
    ),
]


def test_pg_get_viewdef_relabels_a_domain_where_postgresql_coerces_it(tmp_path: Path) -> None:
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        for statement in _SETUP:
            conn.execute(statement)
        got = []
        for i, (query, _, _) in enumerate(VIEWS):
            conn.execute(f"create view v{i} as {query}")
            plain, pretty = conn.execute(
                f"select pg_get_viewdef('v{i}'::regclass), pg_get_viewdef('v{i}'::regclass, true)"
            ).fetchone()
            got.append((query, plain, pretty.replace("\n", " ")))
    assert got == VIEWS


def test_pg_get_viewdef_relabels_a_domain_another_connection_made(tmp_path: Path) -> None:
    """The view texts were printed before the catalog's domains were
    published, so with the domains this thread had published before: a
    domain, table and view made by another connection since the asking one
    last published printed bare, and so did a new connection's first ask,
    on a thread that had published none (RP-1231, seen twice on CI). Every
    answer is PostgreSQL 15.20's (`WANT`, measured 2026-11-28)."""
    want = " SELECT u.k\n   FROM u\n  WHERE ((u.dd)::integer > 2);"
    ask = "select pg_get_viewdef('w'::regclass)"
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        conn.execute("select 1")  # this thread publishes: no domain yet
        with server.connect() as other:
            other.autocommit = True
            other.execute("create domain dint as int")
            other.execute("create table u (k int, dd dint)")
            other.execute("create view w as select k from u where dd > 2")
        got = [conn.execute(ask).fetchone()[0], conn.execute(ask).fetchone()[0]]
        with server.connect() as fresh:
            got.append(fresh.execute(ask).fetchone()[0])
    assert got == [want] * 3
