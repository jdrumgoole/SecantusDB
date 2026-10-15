# reference-version: 15
# RP-015's smaller items: segment and box distances at the float range, "char" ordering by byte, unlogged tables in pg_class and pg_relation_is_publishable.
SELECT pg_relation_is_publishable('smu'::regclass), pg_relation_is_publishable('sm2'::regclass)
SELECT point '(1e300,1e300)' <-> point '(-1e300,-1e300)', point '(3e-310,0)' <-> point '(0,4e-310)', lseg '[(0,0),(1e300,1e300)]' <-> point '(0,1e300)'
SELECT circle '<(1e300,1e300),1>' <-> point '(-1e300,-1e300)', box '(1e300,1e300),(0,0)' <-> point '(-1e300,-1e300)'
SELECT c from (values ('a'::"char"), (E'\\351'::"char"), ('z'::"char"), (E'\\201'::"char")) v(c) order by c
SELECT c from sm2 order by c desc
SELECT c from (values ('b'::"char"), (''::"char"), ('A'::"char")) v(c) order by c
SELECT relname, relpersistence from pg_class where relname in ('smu', 'sm2') order by 1
SELECT int4pl(n, 2) from sm2
SELECT int4pl(k, 2), int4mi(k, 1), textcat(c::text, 'x') from (values (1, 'a'::"char")) v(k, c)
SELECT int4pl(k, 2) from (values (1::int8)) v(k)
SELECT int8pl(k, 2) from (values (1)) v(k)
SELECT int4pl(1, 2), int4pl(1, 2::int2)
select 1 limit 'x'
select 1 limit '2'
select x from (values (1),(2),(3)) v(x) limit '2'
select x from (values (1),(2),(3)) v(x) offset 'y'
select mode() within group (order by NULL)
select mode() within group (order by 'a')
select percentile_disc(0.5) within group (order by NULL)
select percentile_cont(0.5) within group (order by NULL)
select mode() within group (order by x) from (values (1),(1),(2)) v(x)
