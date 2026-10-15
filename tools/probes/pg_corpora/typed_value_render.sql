# reference-version: 15
# A value carried in another form than its text (pg_lsn, money, float4, xid, oid, reg*, interval, the geometric types, bytea, inet, int2vector, timestamptz) printed by its type in ||, concat, format, quote_*, ROW, the JSON functions and arrays; greatest / least / CASE / COALESCE / NULLIF / UNION reading an untyped literal as the typed side's type; casts with no pg_cast row (42846) and operators with no pg_operator candidate (42883), sampled from a 51-type sweep of each.
SET timezone = 'UTC'
select '0/16'::pg_lsn || 'x'
select 'x' || '0/16'::pg_lsn
select concat('0/16'::pg_lsn, '|')
select concat_ws('-', '0/16'::pg_lsn, '0/16'::pg_lsn)
select format('%s|%L|%I', '0/16'::pg_lsn, '0/16'::pg_lsn, '0/16'::pg_lsn)
select quote_literal('0/16'::pg_lsn), quote_nullable('0/16'::pg_lsn)
select row('0/16'::pg_lsn)
select row('0/16'::pg_lsn)::text
select to_json('0/16'::pg_lsn)
select to_jsonb('0/16'::pg_lsn)
select json_build_object('k', '0/16'::pg_lsn)
select json_build_array('0/16'::pg_lsn)
select array_to_json(array['0/16'::pg_lsn])
select greatest('0/16'::pg_lsn, '0/17'), least('0/16'::pg_lsn, '0/17')
select pg_typeof(greatest('0/16'::pg_lsn, '0/17'))
select '1.5'::money || 'x'
select 'x' || '1.5'::money
select concat('1.5'::money, '|')
select concat_ws('-', '1.5'::money, '1.5'::money)
select format('%s|%L|%I', '1.5'::money, '1.5'::money, '1.5'::money)
select quote_literal('1.5'::money), quote_nullable('1.5'::money)
select row('1.5'::money)
select row('1.5'::money)::text
select to_json('1.5'::money)
select to_jsonb('1.5'::money)
select json_build_object('k', '1.5'::money)
select json_build_array('1.5'::money)
select array_to_json(array['1.5'::money])
select array['1.5'::money]
select array['1.5'::money]::text
select greatest('1.5'::money, '2'), least('1.5'::money, '2')
select pg_typeof(greatest('1.5'::money, '2'))
select pg_typeof(greatest('08-00-2b-01-02-03'::macaddr, '08:00:2b:01:02:04'))
select pg_typeof(greatest('08:00:2b:01:02:03:04:05'::macaddr8, '08:00:2b:01:02:03:04:06'))
select concat('pg_class'::regclass, '|')
select concat_ws('-', 'pg_class'::regclass, 'pg_class'::regclass)
select format('%s|%L|%I', 'pg_class'::regclass, 'pg_class'::regclass, 'pg_class'::regclass)
select quote_literal('pg_class'::regclass), quote_nullable('pg_class'::regclass)
select greatest('pg_class'::regclass, 'pg_type'), least('pg_class'::regclass, 'pg_type')
select concat('int4'::regtype, '|')
select concat_ws('-', 'int4'::regtype, 'int4'::regtype)
select format('%s|%L|%I', 'int4'::regtype, 'int4'::regtype, 'int4'::regtype)
select quote_literal('int4'::regtype), quote_nullable('int4'::regtype)
select greatest('int4'::regtype, 'int8'), least('int4'::regtype, 'int8')
select pg_typeof(greatest('int4'::regtype, 'int8'))
select case when false then 'int4'::regtype else 'int8' end
select concat('now'::regproc, '|')
select concat_ws('-', 'now'::regproc, 'now'::regproc)
select format('%s|%L|%I', 'now'::regproc, 'now'::regproc, 'now'::regproc)
select quote_literal('now'::regproc), quote_nullable('now'::regproc)
select array['now'::regproc]
select concat('pg_catalog'::regnamespace, '|')
select concat_ws('-', 'pg_catalog'::regnamespace, 'pg_catalog'::regnamespace)
select format('%s|%L|%I', 'pg_catalog'::regnamespace, 'pg_catalog'::regnamespace, 'pg_catalog'::regnamespace)
select quote_literal('pg_catalog'::regnamespace), quote_nullable('pg_catalog'::regnamespace)
select array['pg_catalog'::regnamespace]
select array['english'::regconfig]
select to_json('5'::xid)
select to_jsonb('5'::xid)
select json_build_object('k', '5'::xid)
select json_build_array('5'::xid)
select array_to_json(array['5'::xid])
select array['5'::xid]
select to_json('5'::xid8)
select to_jsonb('5'::xid8)
select json_build_object('k', '5'::xid8)
select json_build_array('5'::xid8)
select array_to_json(array['5'::xid8])
select array['5'::xid8]
select greatest('5'::xid8, '6'), least('5'::xid8, '6')
select pg_typeof(greatest('5'::xid8, '6'))
select to_json('5'::cid)
select to_jsonb('5'::cid)
select json_build_object('k', '5'::cid)
select json_build_array('5'::cid)
select array_to_json(array['5'::cid])
select array['5'::cid]
select to_json(5::oid)
select to_jsonb(5::oid)
select json_build_object('k', 5::oid)
select json_build_array(5::oid)
select array_to_json(array[5::oid])
select pg_typeof(greatest(5::oid, 6))
select concat(1.1::float4, '|')
select concat_ws('-', 1.1::float4, 1.1::float4)
select format('%s|%L|%I', 1.1::float4, 1.1::float4, 1.1::float4)
select quote_literal(1.1::float4), quote_nullable(1.1::float4)
select row(1.1::float4)
select row(1.1::float4)::text
select to_json(1.1::float4)
select to_jsonb(1.1::float4)
select json_build_object('k', 1.1::float4)
select json_build_array(1.1::float4)
select array_to_json(array[1.1::float4])
select array[1.1::float4]::text
select greatest(1.1::float4, 2.2::float4), least(1.1::float4, 2.2::float4)
select pg_typeof(greatest(1.1::float4, 2.2::float4))
select concat('2020-01-01 10:00+00'::timestamptz, '|')
select concat_ws('-', '2020-01-01 10:00+00'::timestamptz, '2020-01-01 10:00+00'::timestamptz)
select format('%s|%L|%I', '2020-01-01 10:00+00'::timestamptz, '2020-01-01 10:00+00'::timestamptz, '2020-01-01 10:00+00'::timestamptz)
select quote_literal('2020-01-01 10:00+00'::timestamptz), quote_nullable('2020-01-01 10:00+00'::timestamptz)
select row('2020-01-01 10:00+00'::timestamptz)
select row('2020-01-01 10:00+00'::timestamptz)::text
select concat('1 day 2 hours'::interval, '|')
select concat_ws('-', '1 day 2 hours'::interval, '1 day 2 hours'::interval)
select format('%s|%L|%I', '1 day 2 hours'::interval, '1 day 2 hours'::interval, '1 day 2 hours'::interval)
select quote_literal('1 day 2 hours'::interval), quote_nullable('1 day 2 hours'::interval)
select greatest('1 day 2 hours'::interval, '3 days'), least('1 day 2 hours'::interval, '3 days')
select pg_typeof(greatest('1 day 2 hours'::interval, '3 days'))
select concat('\x01ff'::bytea, '|')
select concat_ws('-', '\x01ff'::bytea, '\x01ff'::bytea)
select format('%s|%L|%I', '\x01ff'::bytea, '\x01ff'::bytea, '\x01ff'::bytea)
select quote_literal('\x01ff'::bytea), quote_nullable('\x01ff'::bytea)
select greatest('\x01ff'::bytea, '\x02'), least('\x01ff'::bytea, '\x02')
select quote_literal(true), quote_nullable(true)
select array[B'101']
select pg_typeof(greatest(B'101', B'111'))
select array[B'101'::varbit]
select pg_typeof(greatest(B'101'::varbit, B'111'))
select concat('10.0.0.1/32'::inet, '|')
select concat_ws('-', '10.0.0.1/32'::inet, '10.0.0.1/32'::inet)
select format('%s|%L|%I', '10.0.0.1/32'::inet, '10.0.0.1/32'::inet, '10.0.0.1/32'::inet)
select row('10.0.0.1/32'::inet)
select row('10.0.0.1/32'::inet)::text
select to_json('10.0.0.1/32'::inet)
select to_jsonb('10.0.0.1/32'::inet)
select json_build_object('k', '10.0.0.1/32'::inet)
select json_build_array('10.0.0.1/32'::inet)
select array_to_json(array['10.0.0.1/32'::inet])
select greatest('A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::uuid, 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'), least('A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::uuid, 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11')
select pg_typeof(greatest('A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::uuid, 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'))
select greatest('10:00'::time, '11:00'), least('10:00'::time, '11:00')
select pg_typeof(greatest('10:00'::time, '11:00'))
select greatest('10:00+02'::timetz, '11:00+02'), least('10:00+02'::timetz, '11:00+02')
select pg_typeof(greatest('10:00+02'::timetz, '11:00+02'))
select case when false then '10:00+02'::timetz else '11:00+02' end
select concat('2020-01-01 10:00'::timestamp, '|')
select concat_ws('-', '2020-01-01 10:00'::timestamp, '2020-01-01 10:00'::timestamp)
select format('%s|%L|%I', '2020-01-01 10:00'::timestamp, '2020-01-01 10:00'::timestamp, '2020-01-01 10:00'::timestamp)
select quote_literal('2020-01-01 10:00'::timestamp), quote_nullable('2020-01-01 10:00'::timestamp)
select pg_typeof(greatest('a'::char(3), 'b'))
select pg_typeof(greatest('n'::name, 'o'))
select '(1,2)'::point || 'x'
select 'x' || '(1,2)'::point
select concat('(1,2)'::point, '|')
select concat_ws('-', '(1,2)'::point, '(1,2)'::point)
select format('%s|%L|%I', '(1,2)'::point, '(1,2)'::point, '(1,2)'::point)
select quote_literal('(1,2)'::point), quote_nullable('(1,2)'::point)
select '(1,1),(0,0)'::box || 'x'
select 'x' || '(1,1),(0,0)'::box
select concat('(1,1),(0,0)'::box, '|')
select concat_ws('-', '(1,1),(0,0)'::box, '(1,1),(0,0)'::box)
select format('%s|%L|%I', '(1,1),(0,0)'::box, '(1,1),(0,0)'::box, '(1,1),(0,0)'::box)
select quote_literal('(1,1),(0,0)'::box), quote_nullable('(1,1),(0,0)'::box)
select '[1,3)'::int4range || 'x'
select 'x' || '[1,3)'::int4range
select array['a b'::tsvector]
select concat('1 2'::int2vector, '|')
select concat_ws('-', '1 2'::int2vector, '1 2'::int2vector)
select format('%s|%L|%I', '1 2'::int2vector, '1 2'::int2vector, '1 2'::int2vector)
select quote_literal('1 2'::int2vector), quote_nullable('1 2'::int2vector)
select row('1 2'::int2vector)
select row('1 2'::int2vector)::text
select concat('1 2'::oidvector, '|')
select concat_ws('-', '1 2'::oidvector, '1 2'::oidvector)
select format('%s|%L|%I', '1 2'::oidvector, '1 2'::oidvector, '1 2'::oidvector)
select quote_literal('1 2'::oidvector), quote_nullable('1 2'::oidvector)
select row('1 2'::oidvector)
select row('1 2'::oidvector)::text
select to_json('1 2'::oidvector)
select to_jsonb('1 2'::oidvector)
select json_build_object('k', '1 2'::oidvector)
select json_build_array('1 2'::oidvector)
select array_to_json(array['1 2'::oidvector])
select array['$.a'::jsonpath]
select '{1,2,3}'::line || 'x'
select 'x' || '{1,2,3}'::line
select concat('{1,2,3}'::line, '|')
select concat_ws('-', '{1,2,3}'::line, '{1,2,3}'::line)
select format('%s|%L|%I', '{1,2,3}'::line, '{1,2,3}'::line, '{1,2,3}'::line)
select quote_literal('{1,2,3}'::line), quote_nullable('{1,2,3}'::line)
select '<(1,2),3>'::circle || 'x'
select 'x' || '<(1,2),3>'::circle
select concat('<(1,2),3>'::circle, '|')
select concat_ws('-', '<(1,2),3>'::circle, '<(1,2),3>'::circle)
select format('%s|%L|%I', '<(1,2),3>'::circle, '<(1,2),3>'::circle, '<(1,2),3>'::circle)
select quote_literal('<(1,2),3>'::circle), quote_nullable('<(1,2),3>'::circle)
select '0/16'::pg_lsn union select '0/17' order by 1
select '0/17' union all select '0/16'::pg_lsn
select pg_typeof(x) from (select '0/16'::pg_lsn x union select '0/17') s limit 1
select '0/16'::pg_lsn intersect select '0/17'
select '0/16'::pg_lsn except select '0/17'
select nullif('0/16'::pg_lsn, '0/16')
select nullif('0/16', '0/16'::pg_lsn)
select '1.5'::money union select '2' order by 1
select '2' union all select '1.5'::money
select pg_typeof(x) from (select '1.5'::money x union select '2') s limit 1
select '1.5'::money intersect select '2'
select '1.5'::money except select '2'
select nullif('1.5'::money, '1.5')
select nullif('1.5', '1.5'::money)
select '(0,1)'::tid union select '(0,2)' order by 1
select '(0,2)' union all select '(0,1)'::tid
select pg_typeof(x) from (select '(0,1)'::tid x union select '(0,2)') s limit 1
select '(0,1)'::tid intersect select '(0,2)'
select '(0,1)'::tid except select '(0,2)'
select nullif('(0,1)'::tid, '(0,1)')
select nullif('(0,1)', '(0,1)'::tid)
select '08:00:2b:01:02:03'::macaddr union select '08:00:2b:01:02:04' order by 1
select '08:00:2b:01:02:04' union all select '08:00:2b:01:02:03'::macaddr
select pg_typeof(x) from (select '08:00:2b:01:02:03'::macaddr x union select '08:00:2b:01:02:04') s limit 1
select '08:00:2b:01:02:03'::macaddr intersect select '08:00:2b:01:02:04'
select '08:00:2b:01:02:03'::macaddr except select '08:00:2b:01:02:04'
select '10.0.0.1'::inet union select '10.0.0.2' order by 1
select '10.0.0.2' union all select '10.0.0.1'::inet
select pg_typeof(x) from (select '10.0.0.1'::inet x union select '10.0.0.2') s limit 1
select '10.0.0.1'::inet intersect select '10.0.0.2'
select '10.0.0.1'::inet except select '10.0.0.2'
select nullif('10.0.0.1'::inet, '10.0.0.1')
select nullif('10.0.0.1', '10.0.0.1'::inet)
select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid union select 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11' order by 1
select 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11' union all select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid
select pg_typeof(x) from (select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid x union select 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11') s limit 1
select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid intersect select 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'
select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid except select 'b0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'
select '1 day'::interval union select '2 days' order by 1
select '2 days' union all select '1 day'::interval
select pg_typeof(x) from (select '1 day'::interval x union select '2 days') s limit 1
select '1 day'::interval intersect select '2 days'
select '1 day'::interval except select '2 days'
select nullif('1 day'::interval, '1 day')
select nullif('1 day', '1 day'::interval)
select '2020-01-01'::date union select '2021-01-01' order by 1
select '2021-01-01' union all select '2020-01-01'::date
select pg_typeof(x) from (select '2020-01-01'::date x union select '2021-01-01') s limit 1
select '2020-01-01'::date intersect select '2021-01-01'
select '2020-01-01'::date except select '2021-01-01'
select '2020-01-01 10:00+00'::timestamptz union select '2021-01-01 10:00+00' order by 1
select '2021-01-01 10:00+00' union all select '2020-01-01 10:00+00'::timestamptz
select pg_typeof(x) from (select '2020-01-01 10:00+00'::timestamptz x union select '2021-01-01 10:00+00') s limit 1
select '2020-01-01 10:00+00'::timestamptz intersect select '2021-01-01 10:00+00'
select '2020-01-01 10:00+00'::timestamptz except select '2021-01-01 10:00+00'
select '1'::xid8 union select '2' order by 1
select '2' union all select '1'::xid8
select pg_typeof(x) from (select '1'::xid8 x union select '2') s limit 1
select '1'::xid8 intersect select '2'
select '1'::xid8 except select '2'
select nullif('1'::xid8, '1')
select nullif('1', '1'::xid8)
select 1::oid union select '2' order by 1
select '2' union all select 1::oid
select pg_typeof(x) from (select 1::oid x union select '2') s limit 1
select 1::oid intersect select '2'
select 1::oid except select '2'
select 'pg_class'::regclass union select 'pg_type' order by 1
select 'pg_type' union all select 'pg_class'::regclass
select pg_typeof(x) from (select 'pg_class'::regclass x union select 'pg_type') s limit 1
select 'pg_class'::regclass intersect select 'pg_type'
select 'pg_class'::regclass except select 'pg_type'
select 1::int4 union select '2' order by 1
select '2' union all select 1::int4
select pg_typeof(x) from (select 1::int4 x union select '2') s limit 1
select 1::int4 intersect select '2'
select 1::int4 except select '2'
select 1.5::numeric union select '2.5' order by 1
select '2.5' union all select 1.5::numeric
select pg_typeof(x) from (select 1.5::numeric x union select '2.5') s limit 1
select 1.5::numeric intersect select '2.5'
select 1.5::numeric except select '2.5'
select true::bool union select 'false' order by 1
select 'false' union all select true::bool
select pg_typeof(x) from (select true::bool x union select 'false') s limit 1
select true::bool intersect select 'false'
select true::bool except select 'false'
select '\x01'::bytea union select '\x02' order by 1
select '\x02' union all select '\x01'::bytea
select pg_typeof(x) from (select '\x01'::bytea x union select '\x02') s limit 1
select '\x01'::bytea intersect select '\x02'
select '\x01'::bytea except select '\x02'
select nullif('\x01'::bytea, '\x01')
select nullif('\x01', '\x01'::bytea)
select '(3,4)' union all select '(1,2)'::point
select nullif('(1,2)'::point, '(1,2)')
select nullif('(1,2)'::point, '(3,4)')
select pg_typeof(nullif('(1,2)'::point, '(3,4)'))
select nullif('(1,2)', '(1,2)'::point)
select '1'::jsonb union select '2' order by 1
select '2' union all select '1'::jsonb
select pg_typeof(x) from (select '1'::jsonb x union select '2') s limit 1
select '1'::jsonb intersect select '2'
select '1'::jsonb except select '2'
select B'1'::bit union select '0' order by 1
select '0' union all select B'1'::bit
select pg_typeof(x) from (select B'1'::bit x union select '0') s limit 1
select B'1'::bit intersect select '0'
select B'1'::bit except select '0'
select 1.5::float4 union select '2.5' order by 1
select '2.5' union all select 1.5::float4
select pg_typeof(x) from (select 1.5::float4 x union select '2.5') s limit 1
select 1.5::float4 intersect select '2.5'
select 1.5::float4 except select '2.5'
select '10:00'::time union select '11:00' order by 1
select '11:00' union all select '10:00'::time
select pg_typeof(x) from (select '10:00'::time x union select '11:00') s limit 1
select '10:00'::time intersect select '11:00'
select '10:00'::time except select '11:00'
select nullif('10:00'::time, '10:00')
select nullif('10:00', '10:00'::time)
select '[1,3)'::int4range union select '[2,4)' order by 1
select '[2,4)' union all select '[1,3)'::int4range
select pg_typeof(x) from (select '[1,3)'::int4range x union select '[2,4)') s limit 1
select '[1,3)'::int4range intersect select '[2,4)'
select '[1,3)'::int4range except select '[2,4)'
select (true::bool)::macaddr
select (true::bool)::xid8
select (true::bool)::point
select (true::bool)::line
select (true::bool)::circle
select (true::bool)::int4range
select (1::int4)::date
select (1::int4)::macaddr
select (1::int4)::xid8
select (1::int4)::polygon
select (1::int8)::jsonb
select (1::int8)::point
select (1::int8)::path
select (1.5::float4)::bool
select (1.5::float4)::date
select (1.5::float4)::cid
select (1.5::float4)::regclass
select (1.5::float4)::line
select (1.5::float4)::circle
select (1.5::float8)::bool
select (1.5::float8)::timestamp
select (1.5::float8)::regproc
select (1.5::numeric)::"char"
select (1.5::numeric)::int4range
select (1.5::numeric)::tsquery
select ('1.5'::money)::bool
select ('1'::"char")::bytea
select ('1'::"char")::timestamp
select ('1'::"char")::oid
select ('1'::"char")::varbit
select ('\x01'::bytea)::int4
select ('\x01'::bytea)::int8
select ('\x01'::bytea)::uuid
select ('\x01'::bytea)::varbit
select ('2020-01-01'::date)::float4
select ('2020-01-01'::date)::int4range
select ('10:00'::time)::pg_lsn
select ('10:00'::time)::regclass
select ('10:00+00'::timetz)::uuid
select ('10:00+00'::timetz)::regclass
select ('10:00+00'::timetz)::varbit
select ('10:00+00'::timetz)::point
select ('10:00+00'::timetz)::box
select ('10:00+00'::timetz)::int4range
select ('2020-01-01 10:00'::timestamp)::float8
select ('2020-01-01 10:00'::timestamp)::numeric
select ('2020-01-01 10:00'::timestamp)::tsquery
select ('2020-01-01 10:00+00'::timestamptz)::bool
select ('2020-01-01 10:00+00'::timestamptz)::bytea
select ('2020-01-01 10:00+00'::timestamptz)::macaddr8
select ('2020-01-01 10:00+00'::timestamptz)::polygon
select ('1 day'::interval)::timestamptz
select ('1 day'::interval)::regclass
select ('1 day'::interval)::regproc
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)::int8
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)::xid
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)::box
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)::line
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)::int4range
select ('1'::json)::int2
select ('1'::json)::money
select ('1'::jsonb)::date
select ('1'::jsonb)::timestamptz
select ('1'::jsonb)::regproc
select ('<a/>'::xml)::float8
select ('<a/>'::xml)::"char"
select ('<a/>'::xml)::bytea
select ('<a/>'::xml)::jsonb
select ('<a/>'::xml)::box
select ('10.0.0.1'::inet)::bytea
select ('10.0.0.1'::inet)::bit
select ('10.0.0.1'::inet)::circle
select ('10.0.0.1'::inet)::tsquery
select ('10.0.0.0/8'::cidr)::float8
select ('10.0.0.0/8'::cidr)::jsonb
select ('10.0.0.0/8'::cidr)::xid8
select ('08:00:2b:01:02:03'::macaddr)::timetz
select ('08:00:2b:01:02:03'::macaddr)::timestamptz
select ('08:00:2b:01:02:03:04:05'::macaddr8)::time
select ('08:00:2b:01:02:03:04:05'::macaddr8)::uuid
select ('08:00:2b:01:02:03:04:05'::macaddr8)::point
select ('08:00:2b:01:02:03:04:05'::macaddr8)::line
select ('0/16'::pg_lsn)::int8
select ('0/16'::pg_lsn)::interval
select ('0/16'::pg_lsn)::json
select ('0/16'::pg_lsn)::regclass
select ('0/16'::pg_lsn)::bit
select (1::oid)::bool
select (1::oid)::numeric
select (1::oid)::uuid
select (1::oid)::path
select (1::oid)::int4range
select ('1'::xid)::int2
select ('1'::xid)::numeric
select ('1'::xid)::timestamp
select ('1'::xid)::uuid
select ('1'::xid)::json
select ('1'::xid)::macaddr8
select ('1'::xid)::tsvector
select ('1'::xid8)::jsonb
select ('1'::xid8)::path
select ('1'::cid)::int8
select ('1'::cid)::oid
select ('pg_class'::regclass)::numeric
select ('pg_class'::regclass)::regtype
select ('pg_class'::regclass)::regproc
select ('pg_class'::regclass)::path
select ('int4'::regtype)::timestamp
select ('now'::regproc)::money
select ('pg_catalog'::regnamespace)::int2
select ('pg_catalog'::regnamespace)::date
select ('pg_catalog'::regnamespace)::circle
select (B'1'::bit)::path
select ('(1,2)'::point)::json
select ('(1,2)'::point)::macaddr8
select ('(1,2)'::point)::regclass
select ('(1,1),(0,0)'::box)::float8
select ('(1,1),(0,0)'::box)::bytea
select ('(1,1),(0,0)'::box)::timetz
select ('(1,1),(0,0)'::box)::timestamp
select ('[(0,0),(1,1)]'::lseg)::int8
select ('[(0,0),(1,1)]'::lseg)::xid8
select ('[(0,0),(1,1)]'::lseg)::regproc
select ('[(0,0),(1,1)]'::lseg)::int4range
select ('{1,2,3}'::line)::bool
select ('{1,2,3}'::line)::xml
select ('{1,2,3}'::line)::regproc
select ('<(1,2),3>'::circle)::uuid
select ('<(1,2),3>'::circle)::macaddr
select ('((0,0),(1,1))'::path)::pg_lsn
select ('((0,0),(1,1))'::path)::oid
select ('((0,0),(1,1))'::path)::varbit
select ('((0,0),(1,1))'::path)::int4range
select ('((0,0),(1,1),(1,0))'::polygon)::macaddr
select ('((0,0),(1,1),(1,0))'::polygon)::oid
select ('((0,0),(1,1),(1,0))'::polygon)::cid
select ('[1,3)'::int4range)::bool
select ('[1,3)'::int4range)::date
select ('[1,3)'::int4range)::circle
select ('[1,3)'::int4range)::tsquery
select ('a'::tsvector)::xid8
select ('a'::tsvector)::regclass
select ('a'::tsvector)::regnamespace
select ('a'::tsvector)::bit
select ('a'::tsvector)::circle
select ('a'::tsvector)::path
select ('a'::tsquery)::macaddr
select ('a'::tsquery)::oid
select ('a'::tsquery)::xid
select ('a'::tsquery)::line
select (true::bool) = ('08:00:2b:01:02:03:04:05'::macaddr8)
select (true::bool) = ('0/16'::pg_lsn)
select (true::bool) = ('1'::xid8)
select (true::bool) < ('pg_class'::regclass)
select (1::int2) + ('now'::regproc)
select (1::int4) = ('1'::cid)
select (1::int4) + ('a'::tsvector)
select (1::int8) + ('2020-01-01 10:00+00'::timestamptz)
select (1::int8) < ('1'::json)
select (1::int8) + ('pg_catalog'::regnamespace)
select (1.5::float8) + ('now'::regproc)
select (1.5::float8) < ('pg_catalog'::regnamespace)
select (1.5::float8) + (B'1'::bit)
select (1.5::float8) + ('((0,0),(1,1))'::path)
select (1.5::float8) < ('a'::tsquery)
select (1.5::numeric) = (B'1'::bit)
select (1.5::numeric) < ('(1,1),(0,0)'::box)
select (1.5::numeric) + ('[1,3)'::int4range)
select ('1'::varchar) = ('pg_catalog'::regnamespace)
select ('1'::varchar) = ('(1,2)'::point)
select ('1'::varchar) + ('[(0,0),(1,1)]'::lseg)
select ('1'::bpchar) < ('now'::regproc)
select ('1'::name) = (1::oid)
select ('1'::name) + ('<(1,2),3>'::circle)
select ('1'::"char") < ('now'::regproc)
select ('1'::"char") + (B'1'::varbit)
select ('\x01'::bytea) + ('((0,0),(1,1))'::path)
select ('2020-01-01'::date) = ('10:00+00'::timetz)
select ('2020-01-01'::date) = ('[1,3)'::int4range)
select ('2020-01-01'::date) + ('a'::tsquery)
select ('10:00'::time) < (1::int2)
select ('10:00'::time) < ('1'::varchar)
select ('10:00'::time) < ('\x01'::bytea)
select ('10:00'::time) = ('(1,1),(0,0)'::box)
select ('10:00'::time) + ('{1,2,3}'::line)
select ('10:00'::time) < ('((0,0),(1,1),(1,0))'::polygon)
select ('10:00'::time) < ('a'::tsquery)
select ('10:00+00'::timetz) < (1::int4)
select ('10:00+00'::timetz) = ('<(1,2),3>'::circle)
select ('2020-01-01 10:00'::timestamp) = (B'1'::bit)
select ('2020-01-01 10:00+00'::timestamptz) + ('1'::xid)
select ('1 day'::interval) < ('1.5'::money)
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid) < ('1'::json)
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid) + ('10.0.0.0/8'::cidr)
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid) < ('now'::regproc)
select ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid) = ('a'::tsvector)
select ('1'::json) < ('int4'::regtype)
select ('1'::json) < ('<(1,2),3>'::circle)
select ('1'::jsonb) = ('10:00'::time)
select ('1'::jsonb) < ('10.0.0.0/8'::cidr)
select ('1'::jsonb) + ('08:00:2b:01:02:03:04:05'::macaddr8)
select ('10.0.0.1'::inet) < ('2020-01-01 10:00+00'::timestamptz)
select ('10.0.0.1'::inet) = ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)
select ('10.0.0.1'::inet) + ('1'::jsonb)
select ('08:00:2b:01:02:03'::macaddr) < (1.5::numeric)
select ('08:00:2b:01:02:03'::macaddr) = ('now'::regproc)
select ('08:00:2b:01:02:03'::macaddr) < ('<(1,2),3>'::circle)
select ('08:00:2b:01:02:03'::macaddr) < ('((0,0),(1,1))'::path)
select ('08:00:2b:01:02:03:04:05'::macaddr8) = (1.5::float8)
select ('08:00:2b:01:02:03:04:05'::macaddr8) + ('2020-01-01'::date)
select ('08:00:2b:01:02:03:04:05'::macaddr8) = ('10:00'::time)
select ('08:00:2b:01:02:03:04:05'::macaddr8) < ('1'::jsonb)
select ('08:00:2b:01:02:03:04:05'::macaddr8) + ('[(0,0),(1,1)]'::lseg)
select ('08:00:2b:01:02:03:04:05'::macaddr8) + ('((0,0),(1,1),(1,0))'::polygon)
select ('08:00:2b:01:02:03:04:05'::macaddr8) < ('a'::tsvector)
select ('0/16'::pg_lsn) < (1.5::float4)
select ('0/16'::pg_lsn) = ('1'::text)
select ('0/16'::pg_lsn) < ('1'::varchar)
select ('0/16'::pg_lsn) < ('\x01'::bytea)
select ('0/16'::pg_lsn) = ('1'::json)
select ('0/16'::pg_lsn) = ('08:00:2b:01:02:03'::macaddr)
select ('0/16'::pg_lsn) < ('[(0,0),(1,1)]'::lseg)
select (1::oid) < ('1'::bpchar)
select (1::oid) < ('\x01'::bytea)
select ('1'::xid) < (true::bool)
select ('1'::xid) + (1.5::float8)
select ('1'::xid) < (B'1'::varbit)
select ('1'::xid) + ('(1,2)'::point)
select ('1'::xid8) = (true::bool)
select ('1'::xid8) = ('1'::json)
select ('1'::xid8) < (B'1'::bit)
select ('1'::xid8) + ('[1,3)'::int4range)
select ('1'::cid) = ('\x01'::bytea)
select ('1'::cid) + ('08:00:2b:01:02:03:04:05'::macaddr8)
select ('1'::cid) = ('((0,0),(1,1))'::path)
select ('1'::cid) + ('[1,3)'::int4range)
select ('pg_class'::regclass) < (1.5::float8)
select ('pg_class'::regclass) + (B'1'::varbit)
select ('pg_class'::regclass) + ('<(1,2),3>'::circle)
select ('int4'::regtype) < (1.5::float8)
select ('int4'::regtype) = ('0/16'::pg_lsn)
select ('int4'::regtype) + ('int4'::regtype)
select ('int4'::regtype) < ('(1,2)'::point)
select ('now'::regproc) < ('10:00'::time)
select ('now'::regproc) < ('1'::json)
select ('now'::regproc) + ('10.0.0.1'::inet)
select ('now'::regproc) < ('08:00:2b:01:02:03'::macaddr)
select ('now'::regproc) = ('((0,0),(1,1),(1,0))'::polygon)
select ('now'::regproc) + ('a'::tsvector)
select ('pg_catalog'::regnamespace) = ('1 day'::interval)
select ('pg_catalog'::regnamespace) = ('1'::jsonb)
select ('pg_catalog'::regnamespace) + ('1'::xid)
select ('pg_catalog'::regnamespace) + (B'1'::varbit)
select ('pg_catalog'::regnamespace) < ('((0,0),(1,1))'::path)
select ('pg_catalog'::regnamespace) + ('[1,3)'::int4range)
select (B'1'::bit) = (true::bool)
select (B'1'::bit) < (1::int2)
select (B'1'::bit) + ('10:00'::time)
select (B'1'::bit) = ('1'::cid)
select (B'1'::bit) = ('a'::tsquery)
select (B'1'::varbit) < ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)
select (B'1'::varbit) < ('{1,2,3}'::line)
select (B'1'::varbit) + ('a'::tsquery)
select ('(1,2)'::point) = ('1'::"char")
select ('(1,2)'::point) + ('1'::cid)
select ('(1,1),(0,0)'::box) + (1.5::float8)
select ('(1,1),(0,0)'::box) < ('1'::"char")
select ('(1,1),(0,0)'::box) < ('a'::tsvector)
select ('[(0,0),(1,1)]'::lseg) < ('\x01'::bytea)
select ('[(0,0),(1,1)]'::lseg) < ('[1,3)'::int4range)
select ('{1,2,3}'::line) + (1.5::numeric)
select ('{1,2,3}'::line) < ('1'::varchar)
select ('{1,2,3}'::line) < ('\x01'::bytea)
select ('{1,2,3}'::line) = ('int4'::regtype)
select ('<(1,2),3>'::circle) = ('1 day'::interval)
select ('<(1,2),3>'::circle) = ('08:00:2b:01:02:03:04:05'::macaddr8)
select ('<(1,2),3>'::circle) + ('int4'::regtype)
select ('((0,0),(1,1))'::path) < (1::int8)
select ('((0,0),(1,1))'::path) = ('1'::"char")
select ('((0,0),(1,1))'::path) < ('2020-01-01 10:00+00'::timestamptz)
select ('((0,0),(1,1))'::path) + ('10.0.0.0/8'::cidr)
select ('((0,0),(1,1))'::path) = ('08:00:2b:01:02:03:04:05'::macaddr8)
select ('((0,0),(1,1))'::path) < ('now'::regproc)
select ('((0,0),(1,1))'::path) < ('pg_catalog'::regnamespace)
select ('((0,0),(1,1),(1,0))'::polygon) < ('2020-01-01 10:00'::timestamp)
select ('((0,0),(1,1),(1,0))'::polygon) = ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)
select ('[1,3)'::int4range) + (1::int2)
select ('[1,3)'::int4range) + (1::int4)
select ('[1,3)'::int4range) = ('1'::jsonb)
select ('[1,3)'::int4range) + ('08:00:2b:01:02:03'::macaddr)
select ('[1,3)'::int4range) < ('int4'::regtype)
select ('[1,3)'::int4range) + ('pg_catalog'::regnamespace)
select ('[1,3)'::int4range) < (B'1'::bit)
select ('[1,3)'::int4range) < ('(1,1),(0,0)'::box)
select ('a'::tsvector) < ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)
select ('a'::tsquery) < ('1'::text)
select ('a'::tsquery) = ('1'::json)
select ('a'::tsquery) < ('1'::jsonb)
select ('a'::tsquery) < ('0/16'::pg_lsn)
select ('a'::tsquery) < (B'1'::varbit)
SELECT k, concat(l, '|', m, '|', f, '|', x, '|', i, '|', r, '|', iv, '|', p, '|', b, '|', t) FROM tvr_t ORDER BY k
SELECT k, concat_ws(',', l, m, f, i, r) FROM tvr_t ORDER BY k
SELECT k, format('%s %s %s %L', l, m, i, r) FROM tvr_t ORDER BY k
SELECT k, l || 'x', m || 'x', f || 'x', i || 'x', iv || 'x', p || 'x', t || 'x' FROM tvr_t ORDER BY k
SELECT k, quote_literal(l), quote_nullable(m), quote_literal(i) FROM tvr_t ORDER BY k
SELECT k, row(l, m, f, i, r, t)::text FROM tvr_t ORDER BY k
SELECT k, to_json(l), to_json(m), to_json(f), to_json(x), to_json(i), to_json(r) FROM tvr_t ORDER BY k
SELECT row_to_json(q) FROM (SELECT k, l, m, f, x, t FROM tvr_t ORDER BY k) q
SELECT json_agg(l ORDER BY k), json_agg(m ORDER BY k) FROM tvr_t
SELECT k, greatest(l, '0/20'), least(m, '0'), greatest(f, 2.0::float4) FROM tvr_t ORDER BY k
SELECT k, coalesce(l, '0/1'), coalesce(m, '9'), case when k = 1 then l else '2/2' end FROM tvr_t ORDER BY k
SELECT k, nullif(l, '0/16'), nullif(m, '1.5'), nullif(i, '10.0.0.1/32') FROM tvr_t ORDER BY k
SELECT l FROM tvr_t WHERE k = 1 UNION SELECT '0/17' ORDER BY 1
SELECT m FROM tvr_t WHERE k < 3 UNION ALL SELECT '3' ORDER BY 1
SELECT array_agg(m ORDER BY k)::text, array_agg(f ORDER BY k)::text FROM tvr_t
SELECT l::int8 FROM tvr_t
SELECT m::int8 FROM tvr_t
SELECT k FROM tvr_t WHERE l = 1
SELECT k FROM tvr_t WHERE m = 1.5
SELECT k FROM tvr_t WHERE x = 5
