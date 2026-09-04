SELECT jsonb_path_match('{"a":1}', 'exists($.a)')
SELECT jsonb_path_match('{"a":1}', 'exists($.zz)')
SELECT jsonb_path_match('{"a":1}', '$.a == 1')
SELECT jsonb_path_exists('{"a":[1,2,3]}', '$.a[*] ? (@ > 2)')
SELECT jsonb_path_query('{"a":[1,2,3]}', '$.a[*] ? (@ > 1)')
SELECT jsonb_path_query('{"a":{"b":1},"c":{"b":2}}', '$.*.b')
SELECT count(*) FROM jsonb_path_query('{"a":[1,2,3]}', '$.a[*]') q
SELECT jsonb_path_query_array('{"a":[1,2,3]}', '$.a[*]')
SELECT jsonb_path_query_first('{"a":[1,2,3]}', '$.a[*]')
SELECT jsonb_path_match('{"a":{"b":1}}', 'exists($.a.b)')
SELECT jsonb_path_exists('{"a":1}', 'exists($.a)')
SELECT jsonb_path_exists('{"a":1}', 'exists($.zz)')
SELECT jsonb_path_query('{"a":1}', 'exists($.a)')
SELECT jsonb_path_query('{"a":1}', 'exists($.zz)')
SELECT jsonb_path_match('{"a":1}', 'exists($.zz)')
SELECT jsonb_path_query('{"a":1}', '$.a == 1')
SELECT to_number('1,234.50','9,999.99')
SELECT to_number('12.34','99.99')
SELECT to_number('-12','S99')
SELECT to_number('12-','99MI')
SELECT to_number('1234','9999')
SELECT to_number('  42','999')
SELECT to_number('12%','99%')
SELECT to_number('$12.34','L99.99')
SELECT to_number('123','999D99')
SELECT to_number('1 234','9G999')
SELECT to_number('0012','9999')
SELECT to_number('12','99.99')
SELECT to_number('1.5','9.9')
SELECT to_number('-1.5','S9.9')
SELECT to_number('+12','S99')
SELECT to_number('12','99PR')
SELECT to_number('<12>','99PR')
SELECT to_number('1234567','9999999')
SELECT to_number('abc','999')
SELECT to_number('','999')
SELECT to_number('12.345','99.99')
SELECT to_number('1,2,3','9,9,9')
SELECT to_number('12','0000')
SELECT to_number('00012','00000')
SELECT pg_typeof(to_number('1','9'))
SELECT to_number(NULL,'999')
# --- jsonpath
SELECT jsonb_path_query(j, '$.a.b[*]') FROM sc WHERE id=1
SELECT jsonb_path_query_array(j, '$.a.b[*]') FROM sc WHERE id=1
SELECT jsonb_path_query_first(j, '$.a.b[*]') FROM sc WHERE id=1
SELECT jsonb_path_exists(j, '$.a.b[*] ? (@ > 2)') FROM sc WHERE id=1
SELECT jsonb_path_match(j, 'exists($.a)') FROM sc WHERE id=1
SELECT j @? '$.a.b[*] ? (@ > 2)' FROM sc WHERE id=1
SELECT j @@ '$.c == "x"' FROM sc WHERE id=1
# --- generated / identity columns
SELECT 1
# --- window frame edges
SELECT id, sum(id) OVER (ORDER BY id GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM sc ORDER BY id
SELECT id, first_value(id) OVER (ORDER BY id RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM sc ORDER BY id
# --- MERGE
SELECT 1
# --- grouping sets
SELECT s, count(*) FROM sc GROUP BY GROUPING SETS ((s), ()) ORDER BY s NULLS LAST
SELECT s, count(*), grouping(s) FROM sc GROUP BY CUBE (s) ORDER BY s NULLS LAST
SELECT s, count(*) FROM sc GROUP BY ROLLUP (s) ORDER BY s NULLS LAST
# --- lateral / set ops
SELECT sc.id, x FROM sc, LATERAL unnest(sc.arr) AS x ORDER BY sc.id, x
SELECT id FROM sc INTERSECT SELECT id FROM sc ORDER BY id
SELECT id FROM sc EXCEPT ALL SELECT 1 ORDER BY id
# --- string/format
SELECT format('%s|%I|%L', 'a', 'b c', 'd')
SELECT to_char(ts, 'YYYY-MM-DD HH24:MI:SS') FROM sc ORDER BY id
SELECT to_char(n, '999.99') FROM sc ORDER BY id
SELECT to_number('1,234.50', '9,999.99')
SELECT overlay('abcdef' placing 'XY' from 2 for 3)
# --- ordered-set aggregates
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY n) FROM sc
SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY n) FROM sc
SELECT mode() WITHIN GROUP (ORDER BY s) FROM sc
SELECT rank(2) WITHIN GROUP (ORDER BY id) FROM sc
# --- misc
SELECT * FROM generate_series('2020-01-01'::date, '2020-01-03'::date, '1 day') g
SELECT count(*) FROM information_schema.columns WHERE table_name='sc'
SELECT array_agg(id ORDER BY id DESC) FROM sc
SELECT string_agg(s, ',' ORDER BY id) FROM sc
SELECT xmlelement(name foo, 'bar')
