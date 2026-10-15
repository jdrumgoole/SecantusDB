# reference-version: 15
# query_to_xml / table_to_xml: SQL/XML names, values by type, NULLs, forests, namespaces (RP-015).
select query_to_xml('select * from xmlexp_t order by id', true, false, '')
select query_to_xml('select * from xmlexp_t order by id', false, false, '')
select query_to_xml('select * from xmlexp_t order by id', true, true, '')
select query_to_xml('select * from xmlexp_t order by id', false, true, 'urn:x')
select table_to_xml('xmlexp_t', true, false, '')
select table_to_xml('xmlexp_t'::regclass, false, true, '')
select query_to_xml('select 1 where false', true, false, '')
select query_to_xml('select 1 where false', true, true, '')
select query_to_xml('select 1, 2 as "1a", 3 as "a b", 4 as "é"', true, false, '')
select query_to_xml('select null::int', null, false, '')
select pg_typeof(query_to_xml('select 1', true, false, ''))
select query_to_xml('select ''x''::text as t, '2020-01-01'::date as d, 1.5::numeric as m', true, false, '')
select query_to_xml('select e''a\rb'' as t', true, false, '')
select table_to_xml('nosuch', true, false, '')
select table_to_xml('public.xmlexp_t', false, true, '')
select xmlagg(null::xml)
select xmlagg('<a/>'::xml)::text
select xmlagg(x::xml order by x desc)::text from (values ('<a/>'),('<b/>')) v(x)
select xmlagg(x::xml)::text from (values ('<a/>'),('<b/>')) v(x) where false
select xmlagg(null::xml) filter (where false)
