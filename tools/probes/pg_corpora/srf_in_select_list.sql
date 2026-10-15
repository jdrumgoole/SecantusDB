# reference-version: 15
# Set-returning functions in the select list, ORDER BY, GROUP BY and DISTINCT, as PostgreSQL 15 runs them in a ProjectSet: lockstep calls, nested calls one level per nesting, record-returning calls (json_each, SETOF composite / OUT record) as rows with field selection and (f()).*, and the ProjectSet above aggregation and windows.
select generate_series(1,3) + 1
select upper(unnest(array['a','b']))
select unnest(array[1,2]), generate_series(1,3)
select a, generate_series(1, 2) from sisl_t order by 1, 2
select pg_typeof(json_each('{"a":1}'))::text
select pg_typeof(jsonb_each('{"a":1}'))::text
select json_each('{"a":1,"b":2}')
select jsonb_each('{"a":1,"b":[2]}')
select jsonb_each_text('{"a":1,"b":"x"}')
select json_each_text('{"a":1,"b":"x"}')
select json_each('{"a":1}')::text
select (json_each('{"a":1,"b":2}')).key
select (json_each('{"a":1,"b":2}')).value
select (jsonb_each('{"a":1,"b":2}')).*
select (jsonb_each_text('{"a":1,"b":2}')).*
select pg_typeof((jsonb_each('{"a":1}')).value)::text
select (jsonb_each('{"a":1,"b":2}')).key, 1
select 'x' || (jsonb_each_text('{"a":1,"b":2}')).value
select (json_each('{}')).key
select (json_each(null)).key
select json_each(null)
select a, (jsonb_each(j)).key from sisl_t order by 1, 2
select a, (jsonb_each(j)).* from sisl_t order by 1, 2
select a, jsonb_each(j) from sisl_t order by 1
select a, (jsonb_each_text(j)).value from sisl_t order by 1, 2
select (json_each(js)).key, a from sisl_t order by 2, 1
select jsonb_each(j) as e from sisl_t order by a
select (jsonb_each(j)).key as k from sisl_t where a = 1 order by k
select a, (jsonb_each(j)).key, generate_series(1,3) from sisl_t order by 1, 2, 3
select count(*) from (select jsonb_each(j) from sisl_t) s
select (jsonb_each('{"a":1}')).nosuch
select jsonb_each('{"a":1}') is null
select (jsonb_each('{"a":1,"b":2}')).key from sisl_t where a = 1
select (sisl_pairs(2)).k
select (sisl_pairs(2)).*
select sisl_pairs(2)
select pg_typeof(sisl_pairs(1))::text
select (sisl_recs(2)).*
select (sisl_recs(2)).v + 1
select sisl_recs(2)
select a, (sisl_pairs(a)).v from sisl_t order by 1, 2
select a from sisl_t order by generate_series(1,2), a
select a from sisl_t order by a, generate_series(1,2)
select a from sisl_t order by generate_series(1,2) desc, a
select a from sisl_t group by generate_series(1,2), a order by a
select a, count(*) from sisl_t group by generate_series(1,2), a order by a
select a from sisl_e order by generate_series(1,2)
select a from sisl_t order by unnest(array[3,1]), a
select generate_series(1,3) g order by g desc
select generate_series(1,3) order by generate_series(1,3) desc
select a, generate_series(1,2) as g from sisl_t order by g, a
select a, generate_series(1,2) from sisl_t order by generate_series(1,2) desc, a
select generate_series(1,2) as g, count(*) from sisl_t group by g order by g
select a from sisl_t group by a, unnest(array[1,1]) order by a
select a from sisl_t order by generate_series(1, a), a
select a from sisl_t order by generate_series(1, n), a
select generate_series(1, generate_series(1,2))
select generate_series(1, generate_series(1,3)), 'x'
select generate_series(generate_series(1,2), 3)
select unnest(array[generate_series(1,2), 10])
select generate_series(1,2), generate_series(1, generate_series(1,3))
select generate_series(1, generate_series(1, generate_series(1,2)))
select a, generate_series(1, generate_series(1, a)) from sisl_t order by 1, 2
select generate_series(1, generate_series(0,1))
select generate_series(1, generate_series(1,2)) + generate_series(10,11)
select (jsonb_each(jsonb_build_object('k', generate_series(1,2)))).value
select generate_series(1,2), unnest(array[]::int[])
select generate_series(1,0), 1
select unnest(null::int[]), 1
select generate_series(1,null), 1
select a, generate_series(1, n) from sisl_t order by 1, 2
select a, generate_series(1, n), unnest(array['p','q','r']) from sisl_t order by 1, 2, 3
select count(*), generate_series(1,3) from sisl_t
select sum(a), generate_series(1,2) from sisl_t
select a, generate_series(1,2) from sisl_e
select max(a) + generate_series(1,2) from sisl_t
select a, count(*) over (), generate_series(1,2) from sisl_t order by 1, 3
select distinct generate_series(1,2) from sisl_t order by 1
select generate_series(1,3) limit 2
select a, generate_series(1,2) from sisl_t order by 1, 2 limit 3 offset 1
select (jsonb_each('{"a":1}')).nosuch
select (sisl_pairs(1)).nosuch
select (sisl_recs(1)).nosuch
select a, (jsonb_each(j)).nosuch from sisl_t
select (sisl_pairs(2)).k || 'z', (sisl_pairs(2)).v * 2
select sisl_pairs(2) from sisl_t where a = 1
select (sisl_pairs(a)).* from sisl_t order by 1
select sisl_recs(a) from sisl_t order by a
select row_to_json(jsonb_each('{"a":1}'))
select json_populate_recordset(null::sisl_pair, '[{"k":"a","v":1},{"k":"b"}]')
select (json_populate_recordset(null::sisl_pair, '[{"k":"a","v":1},{"k":"b"}]')).k
select a, (jsonb_populate_recordset(null::sisl_pair, '[{"k":"a","v":1}]')).v from sisl_t order by 1
select (jsonb_each(j)).value -> 'x' from sisl_t where a = 1
select distinct (jsonb_each(j)).key from sisl_t order by 1
select (jsonb_each(j)).key from sisl_t order by 1 limit 1
select a, generate_series(1,2) from sisl_t order by a, generate_series(1,2) desc
select a, generate_series(1,2) g from sisl_t group by a, g order by 1, 2
select a, generate_series(1,2) from sisl_t group by 1, 2 order by 1, 2
select count(*) from sisl_t group by generate_series(1,2)
select generate_series(1,2), count(*) from sisl_t group by generate_series(1,2) order by 1
select generate_series(1, count(*)::int) from sisl_t
select a, generate_series(1, a) from sisl_t group by a order by 1, 2
select a, sum(n), generate_series(1,2) from sisl_t group by a order by 1, 3
select a, count(*), generate_series(1,2) from sisl_t group by a having count(*) > 0 order by 1, 3
select a, generate_series(1,2) from sisl_t group by a order by generate_series(1,2), a
select sum(a), generate_series(1,2) from sisl_e
select count(*), generate_series(1,2) from sisl_e
select a, row_number() over (order by a), generate_series(1,2) from sisl_t order by 1, 3
select a, generate_series(1,2) from sisl_t order by row_number() over (order by a desc), 2
select max(a), unnest(array['x','y']) as u from sisl_t order by u desc
select count(*) + generate_series(1,2) as c from sisl_t order by c
select array_agg(a order by a), generate_series(1,2) from sisl_t
select generate_series(1,2) from sisl_t group by a order by 1
select * from (select generate_series(1, generate_series(1,2)) g) s order by g
select generate_series(1, generate_series(1,2)) union all select 9
select a, generate_series(1, generate_series(1,a)) from sisl_t where a < 3 order by 1, 2
select generate_series(1,2), generate_series(1,2)
select generate_series(1,2) + generate_series(1,3)
select generate_series(1,3) * generate_series(1,3)
select unnest(array[1,2,3]), (jsonb_each('{"a":1}')).key
select pg_typeof(generate_series(1, generate_series(1,2)))::text
select pg_typeof((sisl_recs(1)).k)::text
select pg_typeof(sisl_recs(1))::text
select sisl_pairs(1) is null, (sisl_pairs(1)).k is null
with c as (select 2 as n) select generate_series(1, n) from c
with c as (select 2 as n) select generate_series(1, max(n)) from c
select (select generate_series(1,1)), generate_series(1,2)
select a from sisl_t order by generate_series(1,2), a limit 2
select a, (jsonb_each_text(j)).* from sisl_t order by 1, 2
select (jsonb_each(j)).*, a from sisl_t where j is not null order by 1
select jsonb_each('{"a":{"b":[1,"x"]}}')
select json_each('{"a":null}')
select (json_each_text('{"a":null}')).value is null
select (p).nosuch from sisl_ct
select (sisl_ct.p).nosuch from sisl_ct
select (row('a',1)::sisl_pair).nosuch
select (p).k, (p).* from sisl_ct
select x.a, generate_series(1, y.a) from sisl_t x join sisl_t y on x.a = y.a order by 1, 2
select x.a, (jsonb_each(x.j)).key from sisl_t x, sisl_e order by 1
select x.a, (jsonb_each(x.j)).key from sisl_t x left join sisl_e e on e.a = x.a order by 1, 2
select t.a, unnest(array[t.a, t.n]) from sisl_t t order by 1, 2 nulls first
select distinct on (generate_series(1,2)) a from sisl_t order by generate_series(1,2), a desc
select a, generate_series(1,2) from sisl_t where a in (select generate_series(1,2)) order by 1, 2
select a, (select count(*) from sisl_e), generate_series(1,2) from sisl_t order by 1, 3
select s.k, count(*) from (select (jsonb_each(j)).key k from sisl_t) s group by s.k order by 1
select generate_series(1,2) as a from sisl_t order by a
select generate_series(1,2) as x, a from sisl_t order by x, a
select a, generate_series(a, 3) from sisl_t order by 1, 2
select a, generate_series(1, 2) * a as m from sisl_t order by m, a
select a, string_to_table('a,b', ','), generate_series(1,3) from sisl_t where a = 1
select regexp_split_to_table('a b c', ' ') || 'x'
select length(regexp_split_to_table('a bb ccc', ' '))
select upper((jsonb_each_text('{"a":"x"}')).value)
select (jsonb_each('{"a":1,"b":2}')).key from sisl_t group by 1 order by 1
select count(*), (jsonb_each('{"a":1,"b":2}')).key from sisl_t
select sum(a) over (), (jsonb_each('{"a":1,"b":2}')).key from sisl_t order by 2
select min(a), max(a), generate_series(min(a), max(a)) from sisl_t
select a, generate_series(1, 3) from sisl_t order by 1, 2 limit 4
select count(*) from (select a, generate_series(1, a) from sisl_t) s
select generate_series(1,2), generate_series(1,2)::text
select generate_series(1,2)::text, generate_series(1,2)
select a, (sisl_pairs(a)).k, (sisl_pairs(a)).v from sisl_t order by 1, 2
select a, (sisl_pairs(a)).k, (sisl_pairs(2)).v from sisl_t order by 1, 2, 3
select (sisl_pairs(generate_series(1,2))).k
select (sisl_recs(1)).*, generate_series(1,2)
select generate_series(1,2) from sisl_e group by a
select a, generate_series(1,2) from sisl_t group by a order by a desc, 2 desc
select distinct a, generate_series(1,2) from sisl_t order by 1, 2
