# reference-version: 15
# citext: case-insensitive text from the citext extension -- comparisons,
# ordering, grouping, uniqueness, pattern matching, casts and the catalog.
select 'A'::citext = 'a'::citext
select 'A'::citext = 'a'
select 'A'::citext = 'a'::text
select 'a'::text = 'A'::citext
select 'A'::citext = 'a'::varchar
select 'A'::citext <> 'a'::citext
select 'a'::citext < 'B'::citext
select 'B'::citext > 'a'::citext
select 'abc'::citext <= 'ABC'::citext
select 'Z'::citext >= 'a'::citext
select pg_typeof('x'::citext)
select 'MiXeD'::citext
select 'MiXeD'::citext::text
select null::citext = 'a'::citext
select id, name from ct33_t order by name, id
select id, name from ct33_t order by name desc, id
select count(distinct name) from ct33_t
select lower(name::text), count(*) from ct33_t group by name order by 1
select id from ct33_t where name = 'APPLE' order by id
select id from ct33_t where name in ('apple', 'CHERRY') order by id
select id from ct33_t where name like 'a%' order by id
select id from ct33_t where name like 'A%' order by id
select id from ct33_t where name ilike 'BAN%' order by id
select id from ct33_t where name not like 'b%' order by id
select id from ct33_t where name ~ '^a' order by id
select id from ct33_t where name ~* '^A' order by id
select id from ct33_t where name is null
select min(name), max(name) from ct33_t
select upper(name), lower(name), length(name) from ct33_t where id = 1
select pg_typeof(upper(name)) from ct33_t where id = 1
select string_agg(name, ',' order by id) from ct33_t where id < 4
select pg_typeof(name) from ct33_t where id = 1
INSERT INTO ct33_u VALUES ('HELLO')
INSERT INTO ct33_u VALUES ('world')
select name from ct33_u order by name
select v from ct33_p where k = 'KEY'
INSERT INTO ct33_p VALUES ('OTHER', 3)
select t.id, j.color from ct33_t t join ct33_j j on t.name = j.name order by t.id
select array['A','b']::citext[]
select 'a'::citext = any(array['A','B']::citext[])
select 'abc'::citext || 'DEF'
select length('Hello'::citext)
select 'ß'::citext = 'SS'::citext
select 'É'::citext = 'é'::citext
select 'a'::citext in ('A', 'b')
select 'a'::citext = any('{A,B}'::text[])
select 'a'::citext < 'B'::text
select 'A'::citext::text = 'a'
select pg_typeof('abc'::citext || 'DEF')
select format_type(atttypid, atttypmod) from pg_attribute where attrelid = 'ct33_t'::regclass and attname = 'name'
select typname, typcategory, typlen from pg_type where typname = 'citext'
select 'x'::citext::varchar
select citext 'Foo'
select pg_typeof(coalesce(null::citext, 'X'))
select distinct name from ct33_t order by 1
select name, count(*) from ct33_t group by name order by name
select max('a'::citext), pg_typeof(max('a'::citext))
select strpos('ABC'::citext, 'b'), replace('ABC'::citext, 'b', 'x')
select 'abc'::citext like 'ABC', 'abc'::citext ~ 'B', 'abc'::citext::text ~ 'B'
select array['b','A']::citext[] = array['B','a']::citext[]
select count(*) from ct33_t where name between 'APPLE' and 'B'
select id from ct33_t where name <> 'apple' order by id
UPDATE ct33_j SET color = 'green' WHERE name = 'apple'
select color from ct33_j order by name
DELETE FROM ct33_j WHERE name = 'BANANA'
select count(*) from ct33_j
select extname, extversion from pg_extension where extname = 'citext'
select split_part('aXbxc'::citext, 'x', 2), split_part('a.B.c'::citext, '.', 3)
select regexp_replace('ABC'::citext, 'b', 'x'), regexp_split_to_array('aXbxc'::citext, 'x')
select translate('Hello'::citext, 'l', 'L'), replace('a.A.b'::citext, '.A', '!')
select strpos(name, 'PP') from ct33_t where id = 7
select greatest('a'::citext, 'B'::citext), pg_typeof(least('a'::citext, 'B'::citext))
select 'a'::citext not between 'B' and 'C'
select typinput, typoutput from pg_type where typname = 'citext'
DROP EXTENSION citext
DROP TABLE ct33_t, ct33_u, ct33_p, ct33_j
DROP EXTENSION citext
select 'x'::citext
CREATE EXTENSION citext
