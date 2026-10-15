# reference-version: 15
# A type / domain / range named after a built-in in public (public.uuid beside pg_catalog.uuid); func_select_candidate's unknown-argument steps (unnest, sum, array_agg, ts_headline over untyped literals are 42725) and the Describe type of a call over NULL; width_bucket over a thresholds array of any type; ts_headline's options and its headline selection (hlCover, mark_hl_words, mark_hl_fragments).
SET timezone = 'UTC'
create type uuid as enum ('a', 'b')
select 'a'::public.uuid::text
select pg_typeof('a'::public.uuid)::text
select pg_typeof('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)::text
select 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid::text
select 'a'::uuid
select 'b'::public.uuid > 'a'::public.uuid
select enum_range(null::public.uuid)::text
create type uuid as enum ('c')
create type public.uuid as enum ('c')
create type text as (x int, y text)
select pg_typeof(row(1, 'z')::public.text)::text
select (row(1, 'z')::public.text)::text
select ((row(1, 'z')::public.text)).y
select pg_typeof('x'::text)::text
select 'x'::text || 'y'
create type text as (z int)
create domain int4 as integer check (value > 0)
select pg_typeof(5::public.int4)::text
select pg_typeof(5::int4)::text
select (-1)::int4
select (-1)::public.int4
create domain int4 as int8
create type int8range as range (subtype = int4)
select pg_typeof('[1,2)'::int8range)::text
create type numrange as range (subtype = int4, multirange_type_name = my_numrange_multi)
select pg_typeof('[1,2)'::public.numrange)::text
select pg_typeof('[1,2)'::numrange)::text
select '[1,3)'::public.numrange::text
create table tt (u public.uuid, t uuid, c public.text, d public.int4)
insert into tt values ('a', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', row(1, 'q'), 7)
select pg_typeof(u)::text, pg_typeof(t)::text, pg_typeof(c)::text from tt
select u::text, t::text, c::text, d from tt
insert into tt (d) values (-5)
select 'public.uuid'::regtype::text, 'uuid'::regtype::text, 'public.text'::regtype::text, 'public.int4'::regtype::text
select to_regtype('public.uuid')::text, to_regtype('uuid')::text
select typname, typtype from pg_type t join pg_namespace n on n.oid = t.typnamespace where nspname = 'public' and typname in ('uuid', 'text', 'int4', 'numrange') order by 1
select format_type(t.oid, null) from pg_type t join pg_namespace n on n.oid = t.typnamespace where nspname = 'public' and typname in ('uuid', 'text', 'int4', 'numrange') order by 1
drop table tt
drop type uuid
drop type public.uuid
drop type public.text
drop domain public.int4
drop type public.numrange
select unnest('{1,2}')
select * from unnest('{1,2}')
select unnest('{1,2}'::int[])
select abs(NULL)
select abs('-1')
select abs(NULL) is null
select pg_typeof(abs(NULL))::text
select sqrt(NULL)
select round(NULL)
select round(NULL, 2)
select length(NULL)
select upper(NULL)
select lower('A')
select coalesce(NULL, NULL)
select greatest(NULL, NULL)
select array_length(NULL, 1)
select array_length('{1,2}', 1)
select cardinality('{1,2}')
select array_append('{1}', 2)
select array_append(NULL, 2)
select array_cat('{1}', '{2}')
select width_bucket(5, '{1,3,6}')
select width_bucket(5, '{1,3,6}'::int[])
select width_bucket(5.5, array[1.0, 3.0, 6.0])
select width_bucket(5, array[1, 3, 6])
select width_bucket(0, array[1, 3, 6])
select width_bucket(10, array[1, 3, 6])
select width_bucket(NULL, array[1, 3, 6])
select width_bucket(5, '{1,3,6}'::numeric[])
select width_bucket(5::float8, array[1.5, 3])
select width_bucket('2020-01-02'::date, array['2020-01-01'::date, '2020-02-01'])
select width_bucket(5.35, 0.024, 10.06, 5)
select width_bucket(5, 1, 10, 3)
select width_bucket(5, array[3, 1])
select width_bucket(5, array[[1,2],[3,4]])
select width_bucket(5, array[1, null])
select pg_typeof(width_bucket(5, array[1, 3]))::text
select to_char(NULL)
select md5(NULL)
select concat(NULL)
select octet_length(NULL)
select max(NULL)
select sum(NULL)
select avg(NULL)
select left(NULL, 1)
select abs(1), abs(NULL), sign(NULL)
select log(NULL)
select exp(NULL)
select floor(NULL), ceil(NULL)
select trunc(NULL)
select mod(NULL, 2)
select power(NULL, 2)
select array_upper('{1,2}', 1)
select generate_series(NULL, 2)
select string_to_array(NULL, ',')
select array_to_string('{1,2}', ',')
select array_position('{1,2}', 2)
select array_remove('{1,2}', 2)
select array_ndims('{1,2}')
select array_dims('{1,2}')
select json_typeof(NULL)
select jsonb_typeof(NULL)
select to_json(NULL)
select num_nonnulls(NULL)
select array_agg(NULL)
select array_agg('a')
select max('a')
select min(NULL)
select string_agg(NULL, ',')
select count(NULL)
select bool_and(NULL)
select every(NULL)
select stddev(NULL)
select variance(NULL)
select jsonb_agg(NULL)
select json_agg('x')
select sum('1')
select bit_and(NULL)
select range_agg(NULL)
select percentile_cont(0.5) within group (order by NULL)
select corr(NULL, NULL)
select unnest(NULL)
select array_fill(NULL, '{2}')
select coalesce(unnest('{1}'), 0)
select concat_ws(',', NULL)
select num_nulls(NULL, NULL)
select format('%s', NULL)
select isfinite(NULL)
select date_part('year', NULL)
select extract(year from NULL)
select justify_days(NULL)
select age(NULL)
select to_timestamp(NULL)
select lower(NULL)
select upper('[1,2)')
select isempty(NULL)
select length('abc')
select position('a' in 'abc')
select overlay('abc' placing 'x' from 1)
select substring('abc' from 1 for 1)
select trim('  a ')
select reverse(NULL)
select quote_ident(NULL)
select height(NULL)
select area(NULL)
select center(NULL)
select npoints(NULL)
select host(NULL)
select family(NULL)
select masklen(NULL)
select strip(NULL)
select setweight(NULL, 'A')
select to_tsvector(NULL)
select length(NULL::text)
select abs(NULL::float8)
select floor(NULL::float8)
select mod(NULL::int4, 2)
select stddev(NULL::float8)
select ts_headline('english', 'a b c', 'b')
select ts_headline('a b c', 'b')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'fox'))
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'fox & dog'))
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'fox'), 'StartSel=<, StopSel=>')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'fox'), 'MaxWords=4, MinWords=2')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'fox'), 'MaxWords=5, MinWords=3, ShortWord=2')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'fox'), 'HighlightAll=true')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog. The fox is quick. Another fox here and there.', to_tsquery('english', 'fox'), 'MaxFragments=2, MaxWords=4, MinWords=2')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog. The fox is quick. Another fox here and there.', to_tsquery('english', 'fox'), 'MaxFragments=2, MaxWords=4, MinWords=2, FragmentDelimiter=" | "')
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'cat'))
select ts_headline('english', 'The quick brown fox jumps over the lazy dog', to_tsquery('english', 'cat'), 'MaxWords=3, MinWords=1')
select ts_headline('english', 'one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven twentyeight twentynine thirty thirtyone thirtytwo thirtythree thirtyfour thirtyfive', to_tsquery('english', 'thirty'))
select ts_headline('english', 'one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven twentyeight twentynine thirty thirtyone thirtytwo thirtythree thirtyfour thirtyfive', to_tsquery('english', 'thirty | three'))
select ts_headline('english', 'Search results with highlighted terms for search and results', to_tsquery('english', 'search & result'), 'MaxWords=6, MinWords=3')
select ts_headline('english', 'a b c', 'b', 'MaxWords=1')
select ts_headline('english', 'a b c', 'b', 'MinWords=5')
select ts_headline('english', 'a b c', 'b', 'MaxWords=0')
select ts_headline('english', 'a b c', 'b', 'bogus=1')
select ts_headline('english', 'a b c', 'b', 'MaxFragments=-1')
select ts_headline('english', 'a b c', 'b', 'ShortWord=-1')
drop type if exists public.int8range
