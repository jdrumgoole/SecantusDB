# reference-version: 15
# Function overload choice (func_select_candidate), ungrouped HAVING,
# FROM-less aggregates, inet[] text and aclitem roles.
create table ov_t (a int, b float4, c numeric)
insert into ov_t values (1, 1.5, 2.5), (2, -2.5, 3)
select round(a), pg_typeof(round(a)), ceil(a), floor(b), round(b), abs(b), pg_typeof(abs(b)), round(c) from ov_t order by a
select round(1), pg_typeof(round(1)), pg_typeof(ceil(1::int8)), pg_typeof(trunc(1::int2)), pg_typeof(abs(1.5::float4))
select generate_series(1::int2, 5::int2, 2::int2)
select * from generate_series(1::int2, 3::int2)
select generate_series(1, 3::int2)
select * from generate_series(1::int8, 2::int8)
select generate_series(1::numeric, 2.5::numeric, 0.5)
select generate_series('2020-01-01'::timestamp, '2020-01-02', '12 hours')
select 1 from ov_t having false
select 1 from ov_t having true
select 1 from ov_t having count(*) > 5
select 'x' as k from ov_t where a > 1 having count(*) = 1
select a from ov_t having true
select sum(1)
select count(*) + 1, max(2), min('a'::text)
select pg_typeof(sum(1::int4)), pg_typeof(sum(1::int8)), pg_typeof(avg(1))
select count(*) having false
select count(*) having count(*) = 1
select 1 having true
select 1 having false
select sum(1) where false
select array_agg(3), string_agg('a', ',')
create table ov_n (a inet[], c cidr[])
insert into ov_n values ('{1.2.3.4/32,::1/128,10.0.0.0/8}', '{10.0.0.1/32}')
select a::text, c::text, a::varchar from ov_n
select array['127.0.0.1/32'::inet, '10.0.0.0/8'::inet]::text
create role ov_role
select 'ov_role=r/ov_role'::aclitem::text, '=r/ov_role'::aclitem::text
select 'ov_role=r/ov_role'::aclitem::oid
drop role ov_role
drop table ov_n
drop table ov_t
create table ov_e (a int)
insert into ov_e values (1),(2)
select from ov_e
select from generate_series(1,2)
select 5::oid + 1
select 5::oid - 1::oid
select -(5::oid)
select 5::oid = 5, 5::oid < 7::oid
drop table ov_e
