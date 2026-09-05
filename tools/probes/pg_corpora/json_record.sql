# --- json vs jsonb identity on the functions that return json
SELECT to_json('x'::text)
SELECT to_jsonb('x'::text)
SELECT json_build_object('a', 1)
SELECT jsonb_build_object('a', 1)
SELECT json_build_array(1, 2)
SELECT jsonb_build_array(1, 2)
SELECT json_object('{a,1}')
SELECT to_json(ARRAY[1,2])
SELECT array_to_json(ARRAY[1,2])
SELECT json_agg(a) FROM jr
SELECT jsonb_agg(a) FROM jr
SELECT json_object_agg(b, a) FROM jr
SELECT jsonb_object_agg(b, a) FROM jr
SELECT row_to_json(jr) FROM jr WHERE id=1
SELECT json_strip_nulls('{"a":null,"b":1}')
SELECT jsonb_strip_nulls('{"a":null,"b":1}')
SELECT j FROM jr WHERE id=1
SELECT jb FROM jr WHERE id=1
SELECT pg_typeof(to_json('x'::text)), pg_typeof(to_jsonb('x'::text))
# --- record rendering
SELECT ('a'::text, 1)::text
SELECT ROW('a'::text, 1)::text
SELECT (jr.*)::text FROM jr WHERE id=1
SELECT row_to_json(r) FROM (SELECT 1 AS a, 'b' AS b) r
SELECT row_to_json(t) FROM (SELECT id, a FROM jr WHERE id=1) t
SELECT to_json(r) FROM (SELECT 1 AS a) r
# --- json/jsonb behavioural differences PG keeps
SELECT '{"b":1,"a":2}'::json
SELECT '{"b":1,"a":2}'::jsonb
SELECT '{"a":1,"a":2}'::json
SELECT '{"a":1,"a":2}'::jsonb
SELECT row_to_json(jr) FROM jr WHERE id=1
SELECT row_to_json(t) FROM (SELECT id, a FROM jr WHERE id=1) t
SELECT row_to_json(r) FROM (SELECT 1 AS a, 'b' AS b) r
SELECT to_json(r) FROM (SELECT 1 AS a) r
SELECT (jr.*)::text FROM jr WHERE id=1
SELECT jr FROM jr WHERE id=1
SELECT count(*) FROM jr
SELECT ('a'::text, 1)::text
SELECT ROW('a'::text, 1)::text
SELECT ('a'::text, 'd'::char(2))::text
SELECT (1, 2, 3)::text
SELECT ('a,b'::text, 'c"d'::text)::text
SELECT (NULL::int, 1)::text
SELECT ('', 'x')::text
SELECT ROW(1)::text
SELECT json_strip_nulls('{"a":null,"b":1}')
SELECT json_strip_nulls('{"a":null,"b":1}'::json)
SELECT jsonb_strip_nulls('{"a":null,"b":1}'::jsonb)
SELECT jsonb_strip_nulls('{"a":null,"b":1}')
SELECT json_strip_nulls('[1,null,2]'::json)
SELECT json_strip_nulls('{"a":{"b":null,"c":2}}'::json)
SELECT jb FROM jr WHERE id=1
SELECT jsonb_strip_nulls(jb) FROM jr WHERE id=1
