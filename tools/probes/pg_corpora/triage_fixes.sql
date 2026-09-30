# reference-version: 15
# Findings of the 2026-09-30 backlog triage, each a silent wrong answer or a
# wrong error before: array bounds through greatest/least, min/max(boolean),
# cross-type comparisons on derived columns, function argument types,
# set-operation typing, inet/cidr ordering, comparison and functions,
# DISTINCT ON over a grouped query, and UTF-8 lowercasing in to_tsvector.
select array_lower(greatest('[5:6]={1,2}'::int[], '{0}'::int[]), 1)
select greatest('[5:6]={1,2}'::int[], '{0}'::int[])::text, least('[5:6]={1,2}'::int[], '{9}'::int[])::text
select max(true)
select pg_typeof(x) from (select 'a'::varchar union select 'b'::name) s(x) limit 1
select null::text union select 1
select md5(1)
select substr(1, 1)
select array_length(1, 1)
select to_hex('a'::text)
select * from (select 1 a) s where a = 'x'::text
with c as (select 1 a) select * from c where a = 'x'::text
select * from (select 1 a) s where a + 0 = 'x'::text
create table tf_sm_t (g int)
insert into tf_sm_t values (1), (1), (2)
select distinct on (g) g, count(*) from tf_sm_t group by g order by g
select a::numeric(6,1) z from (values (1.25)) v(a)
select host('192.168.1.5/24'::inet), masklen('192.168.1.5/24'::inet), network('192.168.1.5/24'::inet), broadcast('192.168.1.5/24'::inet)
select family('::1'::inet), abbrev('10.1.0.0/16'::cidr), '192.168.1.5'::inet << '192.168.1.0/24'::inet, set_masklen('10.1.2.3/8'::inet, 16)
select pg_typeof(tstzmultirange(tstzrange('2020-01-01', '2020-02-01')))::text
select tstzmultirange(tstzrange('2020-01-01', '2020-02-01'))::text
select to_tsvector('simple', 'ÄBC ÉTÉ')
drop table tf_sm_t
select * from (select null union select 1) s(x) order by x
select null::text union select 1
select 1 union select null
select pg_typeof(x)::text from (select null union select 1) s(x)
select pg_typeof(x)::text from (select 'a'::varchar union select 'b'::name) s(x) limit 1
select pg_typeof(x)::text from (select 'b'::name union select 'a'::varchar) s(x) limit 1
select pg_typeof(x)::text from (select 'a'::bpchar union select 'b'::text) s(x) limit 1
select pg_typeof(x)::text from (select 'a'::text union select 'b'::bpchar) s(x) limit 1
select pg_typeof(x)::text from (select 'a'::name union select 'b'::text) s(x) limit 1
select pg_typeof(x)::text from (select 'a'::varchar union select 'b'::text) s(x) limit 1
select pg_typeof(x)::text from (select 'a'::text union select 'b'::varchar) s(x) limit 1
create table tf_ni_t (a inet, c cidr)
insert into tf_ni_t values ('10.0.0.1', '10.0.0.0/8'), ('9.0.0.1', '9.0.0.0/8'), ('10.0.0.1/8', '10.0.0.0/16'), ('::1', '::/0'), ('192.168.1.5/24', '192.168.1.0/24'), (null, null)
select a::text as t from tf_ni_t order by a
select c::text as t from tf_ni_t order by c desc
select count(*) from tf_ni_t where a < '9.255.0.0'::inet
select count(*) from tf_ni_t where a >= '10.0.0.1/8'
select max(a)::text, min(c)::text from tf_ni_t
select a::text from tf_ni_t where a = '10.0.0.1'
select a::text from tf_ni_t where a in ('10.0.0.1', '::1') order by a
select count(*) from tf_ni_t where a between '10.0.0.0/8' and '10.255.255.255'
select greatest('10.0.0.1'::inet, '9.0.0.1'::inet)::text
select distinct family(a) from tf_ni_t order by 1
select host(a), masklen(a), network(a)::text, broadcast(a)::text, netmask(a)::text, hostmask(a)::text from tf_ni_t where a is not null order by a
select abbrev(a), abbrev(c), text(a) from tf_ni_t where a is not null order by a
select set_masklen(a, 16)::text, set_masklen(c, 4)::text from tf_ni_t where family(a) = 4 order by a
select inet_same_family(a, '::1'), inet_merge(a, '10.99.0.0/16')::text from tf_ni_t where family(a) = 4 order by a
select a::text from tf_ni_t where a << '10.0.0.0/8'::cidr order by a
select a::text from tf_ni_t where a <<= '10.0.0.0/8' order by a
select c::text from tf_ni_t where c >> '10.0.0.5'::inet order by c
select count(*) from tf_ni_t where a && '0.0.0.0/0'::cidr
select set_masklen('10.1.2.3/8'::inet, 40)
drop table tf_ni_t
create table tf_don_t (g int, h text, v int)
insert into tf_don_t values (1, 'a', 5), (1, 'b', 7), (2, 'a', 1), (2, 'a', 2), (3, 'c', 9)
select distinct on (g) g, count(*) from tf_don_t group by g order by g
select distinct on (g) g, h, sum(v) from tf_don_t group by g, h order by g, sum(v) desc
select distinct on (1) g, h, count(*) as n from tf_don_t group by g, h order by 1, n desc
select distinct on (h) h, max(v) m from tf_don_t group by h order by h, m limit 2
select distinct on (g) g, count(*) from tf_don_t group by g having count(*) > 1 order by g desc
drop table tf_don_t
select 1::inet
select ('10.0.0.1/8'::inet + 5)::text, (5 + '10.0.0.1/8'::inet)::text, ('10.0.0.10'::inet - 3)::text
select '10.0.0.10'::inet - '10.0.0.1'::inet, pg_typeof('10.0.0.10'::inet - '10.0.0.1'::inet)
select (~'10.0.0.1/8'::inet)::text, ('10.1.2.3/8'::inet & '255.0.255.0/16'::inet)::text, ('10.1.2.3/24'::inet | '0.0.0.255/8'::inet)::text
select '255.255.255.255'::inet + 1
select '10.0.0.1'::inet - '::1'::inet
select ('::1'::inet + 1)::text, (~'::1/64'::inet)::text
select ('10.0.0.1'::cidr + 1)::text, pg_typeof('10.0.0.0/8'::cidr + 1)
select '10.0.0.1'::inet & '::1'::inet
select '1.2.3.4'::inet - 5000000000
create table tf_na_t (a inet)
insert into tf_na_t values ('10.0.0.1'), ('10.0.0.9/24')
select (a + 1)::text, a - '10.0.0.0', (a & '255.255.255.0')::text from tf_na_t order by a
drop table tf_na_t
