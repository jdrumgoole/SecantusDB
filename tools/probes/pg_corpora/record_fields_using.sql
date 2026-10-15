# reference-version: 15
# Field selection on an anonymous record (RP-035): fN names only where the ROW() is visible -- directly, or through a FROM subquery / CTE column, typed as the ROW() argument it selects (a nested ROW() a record); a scalar subquery's ROW() and a record field of a record are 42703, a VALUES record 42809. The whole-row value of a subquery whose outputs share a name (RP-033), read by position, with `*` / `q.*` keeping the outputs' names and a shared name 42702. ORDER BY ... USING (RP-025) in a query, an aggregate and a window: < and > (and ~<~ / ~>~) sort, any other operator is 42809, one the type lacks 42883.
select ((select row(1, 'a'))).f1
select (r).f1 from (select row(1, 'a') as r) s
select (x).f1 from (values (row(1, 'a'))) v(x)
with w as (select row(1, 2) r) select (r).f2 from w
select (row(row(1, 2), 3)).f1
select ((row(row(1, 2), 3)).f1).f2
select (row(1, 'a')).f1
select (row(1, 'a')).f3
select pg_typeof((row(1, 'a')).f1)::text
select pg_typeof((r).f1)::text from (select row(1, 'a') as r) s
select (r).f3 from (select row(1, 'a') as r) s
select (r).f1 from (select row(k, v) as r from rfs2_t) s order by 1
select (r).f2 from (select row(k, v) as r from rfs2_t) s order by 1
select (r).k from (select row(k, v) as r from rfs2_t) s
select (r).f1 from (select rfs2_t as r from rfs2_t) s
select (r).k from (select rfs2_t as r from rfs2_t) s order by 1
with w as (select row(1, 2) r) select pg_typeof((r).f2)::text from w
select ((row(row(1, 2), 3)).f1)
select pg_typeof((row(row(1, 2), 3)).f1)::text
select (row(row(1, 2), 3)).f2
select (r).f1 from (select row(row(1, 2), 3) as r) s
select ((r).f1).f2 from (select row(row(1, 2), 3) as r) s
select (x).f1 from (values (row(1, 'a')), (row(2, 'b'))) v(x)
select x from (values (row(1, 'a'))) v(x)
select (x).* from (values (row(1, 'a'))) v(x)
select ((select row(1, 'a'))).*
select (select row(1,'a')).f1
select (r).f1 from (select row(1, 'a') as r union all select row(2, 'b')) s
select (r).f1 from (select row(1, 'a') as r limit 1) s
select (r).f1 from (select distinct row(1, 'a') as r) s
select (rr.r).f1 from (select row(1, 'a') as r) rr
select s.r from (select row(1, 'a') as r) s
select q::text from (select 1, 'a' union select 1, 'a') q
select q from (select 1, 'a' union select 1, 'a') q
select q::text from (select 1, 'a') q
select q::text from (select 1 as x, 2 as x) q
select q from (select 1 as x, 2 as x) q
select row_to_json(q)::text from (select 1, 'a') q
select row_to_json(q)::text from (select 1 as x, 2 as x) q
select row_to_json(q)::text from (select 1, 'a' union select 1, 'a') q
select * from (select 1, 'a' union select 1, 'a') q
select q.* from (select 1, 'a' union select 1, 'a') q
select q.* from (select 1 as x, 2 as x) q
select * from (select 1 as x, 2 as x) q
select q::text from (select a, a from rfs2_w) q order by 1
select q::text from (select a, b, a from rfs2_w) q order by 1
select row_to_json(q)::text from (select a, b, a from rfs2_w) q order by 1
select q::text from (select a, b from rfs2_w) q order by 1
select q::text, q.* from (select 1, 'a') q
select (q).x from (select 1 as x, 2 as x) q
select q::text from (select 1, 'a') q(m, n)
select row_to_json(q)::text from (select 1, 'a') q(m, n)
select q::text from (select 1 as x, 2 as x) q(m)
select q::text from (values (1, 'a')) q
select row_to_json(q)::text from (values (1, 'a')) q
select q::text from (select 1, 2 union all select 3, 4) q order by 1
select count(*) from (select 1, 'a' union select 1, 'a') q where q is not null
select q = q from (select 1, 2) q
select x from (select 1 as x, 2 as x) q
select q.x from (select 1 as x, 2 as x) q
select y, q::text from (select 1 as x, 2 as x, 3 as y) q
select q::text from (select 1 as x, 2 as x, 3 as y) q where y = 3
select q::text from (select 1 as x, 2 as x, 3 as y) q order by y
select count(*), y from (select 1 as x, 2 as x, 3 as y) q where q is not null group by y
select q::text from (select 1 as x, 2 as x) q where x = 1
select *, q::text from (select 1, 'a') q
select q.*, q::text from (select 1, 'a') q
select row_to_json(q)::text from (select a, b, 'z' as a from rfs2_w) q order by 1
select x from (values (1), (3), (2)) v(x) order by x using >
select x from (values (1), (3), (2)) v(x) order by x using <
select string_agg(x::text, ',' order by x using >) from (values (1), (3), (2)) v(x)
select string_agg(x::text, ',' order by x using <) from (values (1), (3), (2)) v(x)
select id from rfs2_o order by v using >
select id from rfs2_o order by v using <
select id from rfs2_o order by v using > nulls first
select id from rfs2_o order by v using < nulls first
select id from rfs2_o order by v using > nulls last
select id from rfs2_o order by s using >, id
select id from rfs2_o order by s using ~>~
select id from rfs2_o order by v using =
select id from rfs2_o order by v using <>
select id from rfs2_o order by v using +
select id from rfs2_o order by p using <
select id from rfs2_o order by v using >=
select id from rfs2_o order by v using <=
select id from rfs2_o order by v using pg_catalog.>
select id from rfs2_o order by v using operator(pg_catalog.<)
select array_agg(id order by v using >) from rfs2_o
select array_agg(id order by v using > nulls last) from rfs2_o
select id, row_number() over (order by v using >) from rfs2_o order by id
select id from rfs2_o order by 1 using >
select id as k from rfs2_o order by k using >
select distinct v from rfs2_o order by v using >
select id from rfs2_o order by v using @@
select * from (select id from rfs2_o order by v using > limit 2) s
select id from rfs2_o union all select 9 order by 1 using >
select v, count(*) from rfs2_o group by v order by v using >
select array_agg(id order by v using =) from rfs2_o
select row_number() over (order by p using <) from rfs2_o
select row_number() over w from rfs2_o window w as (order by v using >) order by 1
select id from rfs2_o order by s using <
select id from rfs2_o order by s using ~<~
select id from rfs2_o order by v using ~<~
select id from rfs2_o order by (v + 1) using >
select string_agg(s, ',' order by s using >) from rfs2_o
select id from rfs2_o order by v using > nulls last, id using <
DROP TABLE rfs2_t, rfs2_w, rfs2_o
