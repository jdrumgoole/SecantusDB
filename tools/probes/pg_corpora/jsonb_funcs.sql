SELECT jsonb_set('{"a":1}', '{b}', '2')
SELECT jsonb_set('{"a":{"b":1}}', '{a,b}', '5', false)
SELECT jsonb_set('{"a":[1,2]}', '{a,0}', '9')
SELECT jsonb_set_lax('{"a":1}', '{a}', NULL, true, 'delete_key')
SELECT jsonb_insert('{"a":[1,2]}', '{a,1}', '9')
SELECT jsonb_insert('{"a":[1,2]}', '{a,1}', '9', true)
SELECT '{"a":1,"b":2}'::jsonb - 'a', '[1,2,3]'::jsonb - 1, '{"a":{"b":1}}'::jsonb #- '{a,b}'
SELECT '{"a":1}'::jsonb || '{"b":2}', '[1]'::jsonb || '[2]'
SELECT jsonb_strip_nulls('{"a":null,"b":1}'), jsonb_pretty('{"a":1}')
SELECT jsonb_object_keys('{"a":1,"b":2}')
SELECT * FROM jsonb_each('{"a":1,"b":"x"}') ORDER BY key
SELECT * FROM jsonb_each_text('{"a":1,"b":"x"}') ORDER BY key
SELECT jsonb_array_elements('[1,"a",{"b":2}]')
SELECT jsonb_array_elements_text('[1,"a"]')
SELECT jsonb_array_length('[1,2,3]'), jsonb_typeof('{"a":1}'), jsonb_typeof('[1]')
SELECT jsonb_build_object('a', 1, 'b', ARRAY[1,2]), jsonb_build_array(1, 'x', NULL)
SELECT jsonb_object('{a,1,b,2}'), jsonb_object('{a,b}', '{1,2}')
SELECT jsonb_agg(x ORDER BY x), jsonb_object_agg(x::text, x * 2) FROM generate_series(1, 3) x
SELECT to_jsonb('x'::text), to_jsonb(1.5), to_jsonb(ARRAY[1,2]), to_jsonb(true)
SELECT row_to_json(r) FROM (SELECT 1 AS a, 'x' AS b) r
SELECT json_build_object('a', 1)::text, json_agg(x) FROM generate_series(1, 2) x
SELECT '{"a":[1,2,3]}'::jsonb @> '{"a":[2]}', '{"a":1}'::jsonb <@ '{"a":1,"b":2}'
SELECT '{"a":1,"b":2}'::jsonb ?| ARRAY['b','c'], '{"a":1,"b":2}'::jsonb ?& ARRAY['a','b']
SELECT jsonb_path_exists('{"a":[1,2]}', '$.a[*] ? (@ > 1)'), jsonb_path_query_first('{"a":[1,2]}', '$.a[1]')
SELECT '{"a":1}'::jsonb @? '$.a', '{"a":1}'::jsonb @@ '$.a == 1'
SELECT jsonb_to_record('{"a":1,"b":"x"}') AS r
SELECT * FROM jsonb_to_record('{"a":1,"b":"x"}') AS t(a int, b text)
SELECT * FROM jsonb_to_recordset('[{"a":1},{"a":2}]') AS t(a int)
SELECT * FROM jsonb_populate_record(NULL::record, '{"a":1}') AS t(a int)
SELECT json_typeof('1'), json_array_length('[1,2]'), json_extract_path_text('{"a":{"b":"c"}}', 'a', 'b'), jsonb_extract_path('{"a":{"b":1}}', 'a')
SELECT '{"a":[{"b":1}]}'::jsonb -> 'a' -> 0 ->> 'b', '{"a":1}'::jsonb ->> 'a', '[1,2]'::jsonb -> -1
SELECT jsonb_path_query('{"a":[1,2,3]}', '$.a[*] ? (@ >= 2)')
SELECT '[1,2]'::jsonb = '[1,2]'::jsonb, '{"b":1,"a":2}'::jsonb::text
SELECT jsonb_build_object('k', NULL) ? 'k', '{"a":1}'::jsonb ? 'b'
SELECT * FROM jsonb_to_record('{"a":1,"b":"x","c":{"d":2},"e":[1]}') AS t(a int, b text, c jsonb, e int[], z text)
SELECT * FROM json_populate_recordset(NULL::record, '[{"a":1},{"a":null}]') AS t(a int)
SELECT jsonb_pretty('{"b":[1,{"c":null}],"a":{}}')
SELECT jsonb_set('{"a":[1,2]}', '{a,-1}', '9'), jsonb_set('{"a":[1,2]}', '{a,5}', '9'), jsonb_set('{"a":[1,2]}', '{a,-9}', '9')
SELECT jsonb_set('{"a":1}', '{x,y}', '2'), jsonb_set('[1]', '{x}', '2')
SELECT jsonb_insert('{"a":1}', '{a}', '2')
SELECT jsonb_set('1', '{a}', '2')
SELECT '{"a":1}'::jsonb - ARRAY['a','b'], '["a","b","a"]'::jsonb - 'a', '{"a":1}'::jsonb - 0
SELECT '{"a":1}'::jsonb || '1'::jsonb, '1'::jsonb || '[2]'::jsonb, pg_typeof('{}'::jsonb || '{}')
SELECT jsonb_array_length('{}')
