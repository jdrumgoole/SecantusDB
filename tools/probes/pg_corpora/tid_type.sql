# reference-version: 15
# The tid type (oid 27): tidin's parsing and errors, tidout, the six comparisons and tideq..tidsmaller, min / max, ORDER BY, casts (only to and from the string types), a tid / tid[] column, and the places a tid is printed (||, concat, format, json, a ROW). A tid is stored as one number, so every line that prints one checks it is not printed as that number.
select '(0,1)'::tid::text
select '(0,1)'::tid
select pg_typeof('(0,1)'::tid)
select ' ( 1 , 2 ) '::tid
select '(1, 2)'::tid
select '( 1,2)'::tid
select '(1,2 )'::tid
select '(4294967295,65535)'::tid
select '(4294967296,1)'::tid
select '(1,65536)'::tid
select '(-1,1)'::tid
select '(1,-1)'::tid
select '(+1,1)'::tid
select '(1,1'::tid
select '1,1)'::tid
select '(1,1)x'::tid
select '(1,1) '::tid
select ' (1,1)'::tid
select '(a,1)'::tid
select '()'::tid
select '(,)'::tid
select '(1)'::tid
select '(1,2,3)'::tid
select '(0x10,1)'::tid
select '(010,1)'::tid
select '(1.5,1)'::tid
select '(1e3,1)'::tid
select ''::tid
select '(  1,2)'::tid
select '(1 ,2)'::tid
select '(-4294967295,1)'::tid
select '(-0,1)'::tid
select '(99999999999999999999,1)'::tid
select '(1,0)'::tid
select '(1,1)'::tid = '(1,1)'
select '(1,1)'::tid < '(1,2)'
select '(1,65535)'::tid < '(2,0)'
select '(1,1)'::tid <> '(1,2)'::tid
select '(2,1)'::tid >= '(1,9)'
select '(1,1)'::tid > '(1,1)'
select '(1,1)'::tid <= '(1,1)'
select tideq('(1,1)','(1,1)'), tidne('(1,1)','(1,2)'), tidlt('(1,1)','(1,2)'), tidle('(1,2)','(1,2)'), tidgt('(3,1)','(1,2)'), tidge('(1,1)','(1,2)')
select tidlarger('(1,1)','(1,2)'), tidsmaller('(1,1)','(1,2)')
select pg_typeof(tidlarger('(1,1)','(1,2)'))
select tidlarger(null,'(1,2)')
select '(1,1)'::tid = null
select null::tid
select pg_typeof(null::tid)
select max(t), min(t) from (values ('(1,1)'::tid),('(2,0)'),('(1,65535)'),(null)) v(t)
select pg_typeof(max(t)) from (values ('(1,1)'::tid)) v(t)
select t from (values ('(1,1)'::tid),('(2,0)'),('(1,65535)'),(null),('(0,5)')) v(t) order by t
select t from (values ('(1,1)'::tid),('(2,0)'),('(1,65535)'),(null),('(0,5)')) v(t) order by t desc
select '(1,1)'::tid || 'x'
select 'x' || '(1,1)'::tid
select concat('(1,1)'::tid, 'a')
select format('%s', '(3,4)'::tid)
select '(1,1)'::tid::varchar
select ('(1,1)'::tid)::text::tid
select '(1,2)'::text::tid
select '(1,1)'::tid::int8
select 1::tid
select '(1,1)'::tid + 1
select array['(1,1)','(2,2)']::tid[]
select '{"(1,1)","(2,3)"}'::tid[]
select pg_typeof('{"(1,1)"}'::tid[])
select ('{"(1,1)","(2,3)"}'::tid[])[2]
select array['(1,1)'::tid, null]
select array['(1,1)'::tid]::text
select tid '(5,6)'
select row('(1,1)'::tid)
select json_build_object('a','(1,1)'::tid)
select to_json('(1,1)'::tid)
select '(1,1)'::tid in ('(1,1)', '(1,2)')
select '(1,1)'::tid between '(0,1)' and '(2,0)'
select distinct t from (values ('(1,1)'::tid),('(1,1)')) v(t)
select count(distinct t) from (values ('(1,1)'::tid),('(1,1)'),('(1,2)')) v(t)
select greatest('(1,1)'::tid, '(1,2)'), least('(1,1)'::tid, '(1,2)')
select coalesce(null::tid, '(9,9)')
select case when true then '(1,1)'::tid end
select tideq('(1,1)', null)
select tideq('x', '(1,1)')
select '(-2147483648,1)'::tid
select '(-2147483649,1)'::tid
select ',5,6)'::tid
select 'x(1,2)'::tid
select '((1,2)'::tid
select E'(\t1,2)'::tid
select '(1,+2)'::tid
select '(- 1,2)'::tid
select '(1,2)'::tid::bpchar
select '(1,2)'::tid::name
select cast('(1,2)' as tid)
select id, t from tid_t order by t, id
select id, t from tid_t order by t desc, id
select id from tid_t where t = '(1,1)'
select id from tid_t where t > '(1,1)' order by id
select id from tid_t where t <= '(1,1)' order by id
select id from tid_t where t <> '(1,1)' order by id
select id from tid_t where t in ('(1,1)', '(2,0)') order by id
select id from tid_t where t between '(0,0)' and '(1,1)' order by id
select id from tid_t where t is null
select min(t), max(t), count(t) from tid_t
select t::text from tid_t order by id
select t || '' from tid_t order by id
select ts from tid_t order by id
select ts::text from tid_t order by id
select ts[1] from tid_t order by id
select pg_typeof(t), pg_typeof(ts) from tid_t where id = 1
select tidlarger(t, '(1,2)') from tid_t order by id
select tideq(t, '(1,1)') from tid_t order by id
select a.id, b.id from tid_t a join tid_t b on a.t = b.t order by 1
select t from tid_t group by t order by t
select t = 1 from tid_t
select '(1,1)'::tid = 1
select '(1,1)'::tid::bigint
select array_agg(t order by t) from tid_t
select string_agg(t::text, ';' order by t) from tid_t
select '(1,)'::tid
select '(,1)'::tid
select '(1,1)'::tid::"char"
select 'a'::"char"::tid
select '(1,1)'::tid::json
select '(1,1)'::tid::tid[]
select 1::bigint::tid
select null::int::tid
select coalesce(t, '(0,0)') from tid_t order by id
select case when id = 1 then t else '(5,5)' end from tid_t order by id
select quote_nullable('(1,1)'::tid)
select quote_literal('(1,1)'::tid)
select length('(1,1)'::tid)
select concat_ws('-', '(1,1)'::tid, t) from tid_t order by id
select json_build_array('(1,1)'::tid)
select to_jsonb('(1,1)'::tid)
select jsonb_build_object('a', t) from tid_t where id = 1
select row(t, id) from tid_t order by id
select array_to_json(array['(1,1)'::tid])
select t from tid_t where t = (select t from tid_t where id = 1)
select max(t) from tid_t where id > 100
select '(1,1)'::tid + '1'
select - '(1,1)'::tid
select '(1,1)'::tid = '(1,1)'::tid
select '(1,1)'::tid < '(1,1)'::text
select t || 1 from tid_t where id = 1
select row('(1,1)'::tid)::text
select row_to_json(row(t)) from tid_t where id = 1
select t || 'a'::text from tid_t where id = 1
select t || t from tid_t where id = 1
select concat_ws('-', '(1,1)'::tid, id) from tid_t order by id
select length(t::text) from tid_t order by id
select upper(t::text) from tid_t order by id
select greatest(t, '(1,2)') from tid_t order by id
select t from tid_t union select '(8,8)'::tid order by 1
select 'tid'::regtype::oid, to_regtype('tid')::oid, to_regtype('tid[]')::oid
select typname, typlen, typcategory from pg_type where oid in (27, 1010) order by oid
select format_type(27, -1), format_type(1010, -1)
select pg_typeof(array['(1,1)'::tid])
select attname, atttypid from pg_attribute where attrelid = 'tid_c'::regclass and attnum > 0 order by attnum
select '(1,1)'::tid is distinct from '(1,2)'
