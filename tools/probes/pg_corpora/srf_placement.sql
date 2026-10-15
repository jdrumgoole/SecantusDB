# Where PostgreSQL 15 allows a set-returning function, and the error where it does not (check_srf_call_placement, transformCaseExpr, transformCoalesceExpr).
select coalesce(unnest(array[1,null,3]), 0)
select case when true then generate_series(1,2) end
select case generate_series(1,2) when 1 then 'a' end
select (case when generate_series(1,2) > 0 then 1 end) from srf_t
select count(generate_series(1,3))
select array_agg(a order by generate_series(1,2)) from srf_t
select count(*) filter (where generate_series(1,2) > 0) from srf_t
select sum(generate_series(1,2)) over () from srf_t
select a from srf_t where generate_series(1,2) > 0
select a from srf_t where abs(generate_series(1,2)) > 0
select sum(a) from srf_t group by a having generate_series(1,2) > 0
select a from srf_t join srf_t s2 on generate_series(1,2) = 1
select a from srf_t limit generate_series(1,2)
select a from srf_t offset generate_series(1,2)
values (generate_series(1,2))
values (1), (generate_series(1,2))
select * from (values (generate_series(1,2))) v
update srf_t set a = generate_series(1,2)
delete from srf_t where generate_series(1,2) > 5
select a from srf_t where (select generate_series(1,2) limit 1) > 0
select a from srf_t where a in (select generate_series(1,2)) order by 1
select generate_series(1,3)
select generate_series(1,3) + 1
select upper(unnest(array['a','b']))
select unnest(array[1,2]), generate_series(1,3)
select greatest(unnest(array[1,5]), 3)
select nullif(unnest(array[1,2]), 1)
select row_number() over (order by generate_series(1,2))
select x from generate_series(1,3) x where x > 1
DROP TABLE IF EXISTS srf_t CASCADE
