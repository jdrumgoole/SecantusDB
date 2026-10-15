# reference-version: 15
# Casts to and from "char": PostgreSQL 15 has int4 <-> "char" (i4tochar / chartoi4) and the string types' I/O casts, nothing for the other numbers or boolean (42846, NULL or not). The call "char"(x) is a FUNCTION -- char(integer) or char(text) -- so an int2 promotes to int4 and an int8 is 42883.
select 65::int2::"char"
select CAST(65::int2 AS "char")
select "char"(65::int2)
select pg_catalog."char"(65::int2)
select pg_typeof("char"(65::int2))::text
select 65::int4::"char"
select CAST(65::int4 AS "char")
select "char"(65::int4)
select pg_catalog."char"(65::int4)
select pg_typeof("char"(65::int4))::text
select 65::int8::"char"
select CAST(65::int8 AS "char")
select "char"(65::int8)
select pg_catalog."char"(65::int8)
select pg_typeof("char"(65::int8))::text
select 65::numeric::"char"
select CAST(65::numeric AS "char")
select "char"(65::numeric)
select pg_catalog."char"(65::numeric)
select pg_typeof("char"(65::numeric))::text
select 65::float4::"char"
select CAST(65::float4 AS "char")
select "char"(65::float4)
select pg_catalog."char"(65::float4)
select pg_typeof("char"(65::float4))::text
select 65::float8::"char"
select CAST(65::float8 AS "char")
select "char"(65::float8)
select pg_catalog."char"(65::float8)
select pg_typeof("char"(65::float8))::text
select 'A'::text::"char"
select CAST('A'::text AS "char")
select "char"('A'::text)
select pg_catalog."char"('A'::text)
select pg_typeof("char"('A'::text))::text
select 'A'::varchar::"char"
select CAST('A'::varchar AS "char")
select "char"('A'::varchar)
select pg_catalog."char"('A'::varchar)
select pg_typeof("char"('A'::varchar))::text
select 'A'::bpchar::"char"
select CAST('A'::bpchar AS "char")
select "char"('A'::bpchar)
select pg_catalog."char"('A'::bpchar)
select pg_typeof("char"('A'::bpchar))::text
select 'ABC'::char(3)::"char"
select CAST('ABC'::char(3) AS "char")
select "char"('ABC'::char(3))
select pg_catalog."char"('ABC'::char(3))
select pg_typeof("char"('ABC'::char(3)))::text
select 'A'::name::"char"
select CAST('A'::name AS "char")
select "char"('A'::name)
select pg_catalog."char"('A'::name)
select pg_typeof("char"('A'::name))::text
select true::"char"
select CAST(true AS "char")
select "char"(true)
select pg_catalog."char"(true)
select pg_typeof("char"(true))::text
select 'A'::"char"::"char"
select CAST('A'::"char" AS "char")
select "char"('A'::"char")
select pg_catalog."char"('A'::"char")
select pg_typeof("char"('A'::"char"))::text
select 'A'::"char"
select CAST('A' AS "char")
select "char"('A')
select pg_catalog."char"('A')
select pg_typeof("char"('A'))::text
select 65::"char"
select CAST(65 AS "char")
select "char"(65)
select pg_catalog."char"(65)
select pg_typeof("char"(65))::text
select 6.5::"char"
select CAST(6.5 AS "char")
select "char"(6.5)
select pg_catalog."char"(6.5)
select pg_typeof("char"(6.5))::text
select '65'::"char"
select CAST('65' AS "char")
select "char"('65')
select pg_catalog."char"('65')
select pg_typeof("char"('65'))::text
select 300::int4::"char"
select CAST(300::int4 AS "char")
select "char"(300::int4)
select pg_catalog."char"(300::int4)
select pg_typeof("char"(300::int4))::text
select -1::int4::"char"
select CAST(-1::int4 AS "char")
select "char"(-1::int4)
select pg_catalog."char"(-1::int4)
select pg_typeof("char"(-1::int4))::text
select (-1)::int4::"char"
select CAST((-1)::int4 AS "char")
select "char"((-1)::int4)
select pg_catalog."char"((-1)::int4)
select pg_typeof("char"((-1)::int4))::text
select 200::int4::"char"
select CAST(200::int4 AS "char")
select "char"(200::int4)
select pg_catalog."char"(200::int4)
select pg_typeof("char"(200::int4))::text
select 0::int4::"char"
select CAST(0::int4 AS "char")
select "char"(0::int4)
select pg_catalog."char"(0::int4)
select pg_typeof("char"(0::int4))::text
select ''::text::"char"
select CAST(''::text AS "char")
select "char"(''::text)
select pg_catalog."char"(''::text)
select pg_typeof("char"(''::text))::text
select 'ABC'::text::"char"
select CAST('ABC'::text AS "char")
select "char"('ABC'::text)
select pg_catalog."char"('ABC'::text)
select pg_typeof("char"('ABC'::text))::text
select 'é'::text::"char"
select CAST('é'::text AS "char")
select "char"('é'::text)
select pg_catalog."char"('é'::text)
select pg_typeof("char"('é'::text))::text
select null::int4::"char"
select CAST(null::int4 AS "char")
select "char"(null::int4)
select pg_catalog."char"(null::int4)
select pg_typeof("char"(null::int4))::text
select null::text::"char"
select CAST(null::text AS "char")
select "char"(null::text)
select pg_catalog."char"(null::text)
select pg_typeof("char"(null::text))::text
select null::"char"
select CAST(null AS "char")
select "char"(null)
select pg_catalog."char"(null)
select pg_typeof("char"(null))::text
select 'A'::"char"::int2
select pg_typeof('A'::"char"::int2)::text
select '\\001'::"char"::int2
select pg_typeof('\\001'::"char"::int2)::text
select 'é'::"char"::int2
select pg_typeof('é'::"char"::int2)::text
select ''::"char"::int2
select pg_typeof(''::"char"::int2)::text
select null::"char"::int2
select pg_typeof(null::"char"::int2)::text
select int2('A'::"char")
select 'A'::"char"::int4
select pg_typeof('A'::"char"::int4)::text
select '\\001'::"char"::int4
select pg_typeof('\\001'::"char"::int4)::text
select 'é'::"char"::int4
select pg_typeof('é'::"char"::int4)::text
select ''::"char"::int4
select pg_typeof(''::"char"::int4)::text
select null::"char"::int4
select pg_typeof(null::"char"::int4)::text
select int4('A'::"char")
select 'A'::"char"::int8
select pg_typeof('A'::"char"::int8)::text
select '\\001'::"char"::int8
select pg_typeof('\\001'::"char"::int8)::text
select 'é'::"char"::int8
select pg_typeof('é'::"char"::int8)::text
select ''::"char"::int8
select pg_typeof(''::"char"::int8)::text
select null::"char"::int8
select pg_typeof(null::"char"::int8)::text
select int8('A'::"char")
select 'A'::"char"::numeric
select pg_typeof('A'::"char"::numeric)::text
select '\\001'::"char"::numeric
select pg_typeof('\\001'::"char"::numeric)::text
select 'é'::"char"::numeric
select pg_typeof('é'::"char"::numeric)::text
select ''::"char"::numeric
select pg_typeof(''::"char"::numeric)::text
select null::"char"::numeric
select pg_typeof(null::"char"::numeric)::text
select numeric('A'::"char")
select 'A'::"char"::float4
select pg_typeof('A'::"char"::float4)::text
select '\\001'::"char"::float4
select pg_typeof('\\001'::"char"::float4)::text
select 'é'::"char"::float4
select pg_typeof('é'::"char"::float4)::text
select ''::"char"::float4
select pg_typeof(''::"char"::float4)::text
select null::"char"::float4
select pg_typeof(null::"char"::float4)::text
select float4('A'::"char")
select 'A'::"char"::float8
select pg_typeof('A'::"char"::float8)::text
select '\\001'::"char"::float8
select pg_typeof('\\001'::"char"::float8)::text
select 'é'::"char"::float8
select pg_typeof('é'::"char"::float8)::text
select ''::"char"::float8
select pg_typeof(''::"char"::float8)::text
select null::"char"::float8
select pg_typeof(null::"char"::float8)::text
select float8('A'::"char")
select 'A'::"char"::text
select pg_typeof('A'::"char"::text)::text
select '\\001'::"char"::text
select pg_typeof('\\001'::"char"::text)::text
select 'é'::"char"::text
select pg_typeof('é'::"char"::text)::text
select ''::"char"::text
select pg_typeof(''::"char"::text)::text
select null::"char"::text
select pg_typeof(null::"char"::text)::text
select text('A'::"char")
select 'A'::"char"::varchar
select pg_typeof('A'::"char"::varchar)::text
select '\\001'::"char"::varchar
select pg_typeof('\\001'::"char"::varchar)::text
select 'é'::"char"::varchar
select pg_typeof('é'::"char"::varchar)::text
select ''::"char"::varchar
select pg_typeof(''::"char"::varchar)::text
select null::"char"::varchar
select pg_typeof(null::"char"::varchar)::text
select varchar('A'::"char")
select 'A'::"char"::bpchar
select pg_typeof('A'::"char"::bpchar)::text
select '\\001'::"char"::bpchar
select pg_typeof('\\001'::"char"::bpchar)::text
select pg_typeof('é'::"char"::bpchar)::text
select ''::"char"::bpchar
select pg_typeof(''::"char"::bpchar)::text
select null::"char"::bpchar
select pg_typeof(null::"char"::bpchar)::text
select bpchar('A'::"char")
select 'A'::"char"::name
select pg_typeof('A'::"char"::name)::text
select '\\001'::"char"::name
select pg_typeof('\\001'::"char"::name)::text
select 'é'::"char"::name
select pg_typeof('é'::"char"::name)::text
select ''::"char"::name
select pg_typeof(''::"char"::name)::text
select null::"char"::name
select pg_typeof(null::"char"::name)::text
select name('A'::"char")
select 'A'::"char"::bool
select pg_typeof('A'::"char"::bool)::text
select '\\001'::"char"::bool
select pg_typeof('\\001'::"char"::bool)::text
select 'é'::"char"::bool
select pg_typeof('é'::"char"::bool)::text
select ''::"char"::bool
select pg_typeof(''::"char"::bool)::text
select null::"char"::bool
select pg_typeof(null::"char"::bool)::text
select bool('A'::"char")
select 'A'::"char"::"char"
select pg_typeof('A'::"char"::"char")::text
select '\\001'::"char"::"char"
select pg_typeof('\\001'::"char"::"char")::text
select 'é'::"char"::"char"
select pg_typeof('é'::"char"::"char")::text
select ''::"char"::"char"
select pg_typeof(''::"char"::"char")::text
select null::"char"::"char"
select pg_typeof(null::"char"::"char")::text
select "char"('A'::"char")
select 'A'::"char"::integer
select pg_typeof('A'::"char"::integer)::text
select '\\001'::"char"::integer
select pg_typeof('\\001'::"char"::integer)::text
select 'é'::"char"::integer
select pg_typeof('é'::"char"::integer)::text
select ''::"char"::integer
select pg_typeof(''::"char"::integer)::text
select null::"char"::integer
select pg_typeof(null::"char"::integer)::text
select integer('A'::"char")
select 'A'::"char"::smallint
select pg_typeof('A'::"char"::smallint)::text
select '\\001'::"char"::smallint
select pg_typeof('\\001'::"char"::smallint)::text
select 'é'::"char"::smallint
select pg_typeof('é'::"char"::smallint)::text
select ''::"char"::smallint
select pg_typeof(''::"char"::smallint)::text
select null::"char"::smallint
select pg_typeof(null::"char"::smallint)::text
select smallint('A'::"char")
select 'A'::"char"::bigint
select pg_typeof('A'::"char"::bigint)::text
select '\\001'::"char"::bigint
select pg_typeof('\\001'::"char"::bigint)::text
select 'é'::"char"::bigint
select pg_typeof('é'::"char"::bigint)::text
select ''::"char"::bigint
select pg_typeof(''::"char"::bigint)::text
select null::"char"::bigint
select pg_typeof(null::"char"::bigint)::text
select bigint('A'::"char")
select i2::"char", i4::"char", t::"char" from chc_t
select "char"(i2), "char"(i4), "char"(t) from chc_t
select "char"(i8) from chc_t
select i8::"char" from chc_t
select n::"char" from chc_t
select "char"(n) from chc_t
select c::int4, c::text, c::int2 from chc_t
select int4(c), text(c) from chc_t
select b::"char" from chc_t
select "char"(b) from chc_t
select "char"(v) from chc_t
select v::"char" from chc_t
select i2::"char" from chc_e
select "char"(i2) from chc_e
select "char"(i8) from chc_t where false
select c::int8 from chc_e
select int8(c) from chc_e
select i4::"char", "char"(i4), "char"(i2) from chc_t order by 1
select "char"(i2)::int4 from chc_t order by 1
select "char"(200)
select "char"(-1)
select "char"(127), "char"(-128), "char"(128)
select "char"(65::int2)
select "char"(300::int2)
select "char"('')
select "char"('\101')
select i2 from chc_t where i2::"char" = 'B'
select * from chc_t where "char"(i2) = 'B'
insert into chc_d values (65::int2)
insert into chc_d values (65)
insert into chc_d values ('Q')
select c::text, c::varchar, c::name, c::bpchar from chc_t order by 1
select "char"(v), "char"(c) from chc_t order by 1
select pg_typeof("char"(i2))::text from chc_t limit 1
select cast(i2 as "char") from chc_t
select cast(c as int2) from chc_t
select coalesce(i2::"char", 'x') from chc_t
select char(65)
select "char"(65, 66)
select "char"()
# Assignments into a "char" column, and of a "char" into other columns: only the string types have an assignment cast.
insert into chc_d values (65::int2)
insert into chc_d values (65)
insert into chc_d values (65::int8)
insert into chc_d values (true)
insert into chc_d values ('Q')
insert into chc_d values ('Q'::text)
insert into chc_d values ('Q'::varchar)
insert into chc_d select t from chc_t
insert into chc_d select c from chc_t
insert into chc_n values ('A'::"char", null, null, null)
insert into chc_n values (null, 'A'::"char", null, null)
insert into chc_n values (null, null, 'A'::"char", null)
insert into chc_n values (null, null, null, 'A'::"char")
insert into chc_n (t) select c from chc_t
