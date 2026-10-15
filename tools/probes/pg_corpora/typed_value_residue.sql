# reference-version: 15
# What the 2026-10-15 typed-value work left (RP-025) and a table named after a built-in type (RP-030): an array of records; regprocedure over the built-ins, regoper and regoperator; a refused function-style cast is a missing function; ORDER BY, DISTINCT, GROUP BY, windows, aggregates and set operations over types with no ordering or equality operator (point, json, xml, box, arrays of them); information_schema.columns data_type / udt_name; path comparisons; time against timetz and interval; macaddr against macaddr8 (ambiguous); input errors at the literal, and none for a value cast from text; a table `tid` / `xid` whose row type is public.tid while a bare tid stays pg_catalog's.
SET timezone = 'UTC'
select array[row(1)]
select array[row(1)]::text
select pg_typeof(array[row(1)])::text
select array[row(1,'a'), row(2,'b')]
select (array[row(1)])[1]
select 'now()'::regprocedure::text
select 'now'::regprocedure::text
select 'abs(int)'::regprocedure::text
select 'nosuch()'::regprocedure::text
select '+(int,int)'::regoperator::text
select '+'::regoper::text
select '||/'::regoper::text
select pg_typeof('+'::regoper)::text
select '-'::regoper::text
select 'nosuch'::regoper::text
select 551::regoper::text
select int8(true)
select int4(true)
select int8(1.5)
select text(1)
select bool(1)
select int8('5')
select p from (values (point '(1,1)'), (point '(0,0)')) v(p) order by p
select point '(1,1)' union select point '(0,0)'
select point '(1,1)' union all select point '(0,0)'
select point '(1,1)' intersect select point '(0,0)'
select point '(1,1)' except select point '(0,0)'
select distinct p from (values (point '(1,1)')) v(p)
select p from (values (point '(1,1)')) v(p) group by p
select '1 2'::int2vector = '1 2'::int2vector
select '1 2'::oidvector = '1 2'::oidvector
select '((0,0),(1,1))'::path = '((0,0),(1,1))'::path
select '1 2'::int2vector <> '1 3'::int2vector
select '1 2'::oidvector < '1 3'::oidvector
select time '01:00' = timetz '01:00+00'
select time '01:00' < timetz '01:00+00'
select time '01:00' = interval '1 hour'
select time '01:00' < interval '1 hour'
select pg_typeof(time '01:00' + interval '1 hour')::text
select macaddr '08:00:2b:01:02:03' = macaddr8 '08:00:2b:01:02:03:04:05'
select macaddr '08:00:2b:01:02:03' < macaddr8 '08:00:2b:ff:fe:01:02:03'
select macaddr '08:00:2b:01:02:03' = macaddr8 '08:00:2b:ff:fe:01:02:03'
select 'abc'::int
select '  abc'::int
select 'x'::date
select 1, 'x'::int
select int4 'x'
select cast('x' as int)
select column_name, data_type, udt_name from information_schema.columns where table_name = 'tvr2_arr' order by ordinal_position
select '+(int4,int4)'::regoperator::text
select '-(NONE,int4)'::regoperator::text
select '-(none,integer)'::regoperator
select '+(int4)'::regoperator
select '+(int4,int4,int4)'::regoperator
select '+(text,text)'::regoperator
select 551::regoperator::text
select 551::regoper
select '+(int4,int4)'::regoperator::oid
select '||/'::regoper::oid
select pg_typeof('||/'::regoper)::text
select pg_typeof('+(int4,int4)'::regoperator)::text
select 'nosuch'::regproc
select 'abs'::regproc
select 'now'::regproc::text
select 'abs(float8)'::regprocedure
select 'abs(double precision)'::regprocedure::oid
select 'pg_catalog.abs(numeric)'::regprocedure::text
select 'char(int)'::regprocedure::text
select 'position(text,text)'::regprocedure::text
select 'array_length(anyarray,int)'::regprocedure::text
select 'concat("any")'::regprocedure::text
select 'abs(int'::regprocedure
select 1242::regprocedure::text
select 2::regprocedure::text
select timetz '01:00+00' = timetz '02:00+01'
select time '02:00' > timetz '01:00+00'
select time '01:00' <= interval '1 hour'
select time '01:30' > interval '1 hour'
select interval '1 hour' = time '01:00'
select timetz '01:00+00' = time '01:00'
select '((0,0),(1,1))'::path < '((0,0),(1,1),(2,2))'::path
select '((0,0),(1,1))'::path >= '[(0,0),(1,1)]'::path
select '((0,0),(1,1))'::path <> '((0,0),(1,1))'::path
select macaddr '08:00:2b:01:02:03' = macaddr '08:00:2b:01:02:03'
select macaddr8 '08:00:2b:01:02:03:04:05' > macaddr8 '08:00:2b:01:02:03:04:04'
select macaddr '08:00:2b:01:02:03'::macaddr8 = macaddr8 '08:00:2b:ff:fe:01:02:03'
select int2(true)
select float8(true)
select text(true)
select int8('x'::text)
select bool(1.5)
select lseg(box '(1,1),(0,0)')
select point(box '(1,1),(0,0)')
select circle(box '(1,1),(0,0)')
select int2 = 1 from (values (1::int2)) v(int2)
select 'x'::text::int
select cast('x'::text as int)
select 'x'::varchar::int
select ('x'::text)::int
select 'x'::name::int
select int4('x')
select '1.5x'::numeric
select array_length(array[row(1),row(2)], 1)
select array[row(1)] = array[row(1)]
select cardinality(array[row(1,'a')])
select array[row(1,null)]
select array[row(1)] is null
select array[row(1), null]
select array[(1,'x')]
select array[]::record[]
select column_name, data_type, udt_name from information_schema.columns where table_name = 'tvr2_cols' order by ordinal_position
select column_name, data_type, udt_name from information_schema.columns where table_name = 'tvr2_v' order by ordinal_position
select p from tvr2_pt order by 1
select p as q from tvr2_pt order by q
select p from tvr2_pt order by p desc
select id from tvr2_pt order by p
select id from tvr2_pt order by p, id
select id from tvr2_pt order by id, j
select id from tvr2_pt order by tvr2_pt.p
select id from tvr2_pt order by x
select id from tvr2_pt order by a
select distinct a from tvr2_pt
select distinct id, p from tvr2_pt
select distinct on (p) id from tvr2_pt
select distinct on (id) id, p from tvr2_pt
select id from tvr2_pt group by id, p
select p from tvr2_pt group by 1
select p from tvr2_pt union select p from tvr2_pt
select id, p from tvr2_pt union select id, p from tvr2_pt
select p from tvr2_pt union select null
select null union select p from tvr2_pt
select p from tvr2_pt intersect all select p from tvr2_pt
select p from tvr2_pt except all select p from tvr2_pt
select j from tvr2_pt union select j from tvr2_pt
select b from tvr2_pt union select b from tvr2_pt
select count(distinct p) from tvr2_pt
select array_agg(p order by p) from tvr2_pt
select row_number() over (order by p) from tvr2_pt
select row_number() over (partition by p) from tvr2_pt
select j::text from tvr2_pt order by j::text
select p from tvr2_pt order by p <-> point '(0,0)'
select id from tvr2_pt where p = p
select p from tvr2_pt order by 1 desc nulls first
select * from (select p from tvr2_pt) s order by p
select * from (select p from tvr2_pt order by p) s
select p from tvr2_pt order by p limit 1
select id from tvr2_pt order by (p)
select row(p) from tvr2_pt order by 1
select '(0,1)'::tid::text
select pg_typeof('(0,1)'::tid)::text
select row_to_json(x)::text from tid x
select row_to_json(tid)::text from tid
select x::text from tid x
select pg_typeof(x)::text from tid x
select '(2020-01-01)'::public.tid::text
select pg_typeof('(2020-01-01)'::public.tid)::text
select row('2020-01-02'::date)::public.tid
select to_regtype('tid')::text
select to_regtype('public.tid')::text
select 'tid'::regtype::oid = 27
select 'public.tid'::regtype::oid = 27
select pg_typeof(array['(0,1)'::tid])::text
select '{"(0,1)"}'::tid[]
select x from tid x
select (x).d from tid x
select x.d from tid x
select tid from tid
select tid.* from tid
select '(0,1)'::tid = '(0,1)'::tid
select null::tid
select pg_typeof(null::public.tid)::text
select row_to_json(row('2020-01-02'::date)::public.tid)::text
select (row('2020-01-02'::date)::public.tid).d
select json_populate_record(null::public.tid, '{"d":"2020-03-04"}')::text
select * from json_populate_record(null::public.tid, '{"d":"2020-03-04"}')
select * from tid
select * from xid
select row_to_json(x)::text from xid x
select pg_typeof(x)::text from xid x
select '5'::xid::text
select pg_typeof('5'::xid)::text
select '(2,y)'::public.xid::text
select pg_typeof(r)::text, pg_typeof(e)::text from tvr2_holder
select r::text, e::text from tvr2_holder
select (r).d from tvr2_holder
select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid::text
select to_regtype('uuid')::text
select array[x] from tid x
select pg_typeof(array[x])::text from tid x
select x = x from tid x
select count(*) from tid x where x is not null
select typname from pg_type where typname in ('tid','xid','uuid') order by typname, typnamespace
select format_type(oid, null) from pg_type where typname in ('tid','xid','uuid') order by typnamespace, typname
select 'public.tid'::regtype::text
select 'tid'::regtype::text
select array[row(1)] = array[row(2)]
select array[row(1)] < array[row(2)]
select array[row(1)] <> array[row(1), row(2)]
select array[row(1)] < array[row(1), row(2)]
set timezone = 'America/New_York'
select time '01:00' = timetz '01:00+00'
select time '01:00' = timetz '01:00-04'
select time '01:00'::timetz
select time '02:00' > timetz '01:00+00'
set timezone = 'Asia/Kolkata'
select time '01:00' = timetz '01:00+05:30'
SET timezone = 'UTC'
DROP TABLE tvr2_holder
DROP TABLE tid
DROP TABLE xid

