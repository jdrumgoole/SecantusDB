# Arrays whose lower bound is not 1, measured against PostgreSQL 14.
SELECT '[0:1]={a,b}'::text[]::text, array_lower('[0:1]={a,b}'::text[],1), array_upper('[0:1]={a,b}'::text[],1), array_dims('[0:1]={a,b}'::text[])
SELECT ('[0:1]={a,b}'::text[])[0], ('[0:1]={a,b}'::text[])[1], ('[0:1]={a,b}'::text[])[2]
SELECT ('[0:2]={a,b,c}'::text[])[0:1]::text, ('[0:2]={a,b,c}'::text[])[1:]::text
SELECT '[0:1]={a,b}'::text[] = '{a,b}'::text[], '[0:1]={a,b}'::text[] < '{a,b}'::text[], '[0:1]={a,b}'::text[] > '{a,b}'::text[], '[0:1]={a,b}'::text[] = '[0:1]={a,b}'::text[]
SELECT array_append('[0:1]={a,b}'::text[], 'c')::text, array_prepend('z', '[0:1]={a,b}'::text[])::text
SELECT ('[0:1]={a,b}'::text[] || 'c'::text)::text, ('[0:1]={a,b}'::text[] || '{c}'::text[])::text, ('{c}'::text[] || '[0:1]={a,b}'::text[])::text
SELECT array_length('[0:1]={a,b}'::text[],1), cardinality('[0:1]={a,b}'::text[]), array_to_string('[0:1]={a,b}'::text[], ','), 'a' = ANY('[0:1]={a,b}'::text[])
SELECT array_fill(7, ARRAY[2], ARRAY[3])::text, array_fill(1, ARRAY[2,2], ARRAY[0,5])::text
SELECT array_fill(7, ARRAY[2], ARRAY[3])
SELECT '[1:2]={a,b}'::text[]::text, '[-2:-1][1:2]={{1,2},{3,4}}'::int[]::text
SELECT array_position('[0:2]={a,b,c}'::text[], 'b'), array_positions('[0:2]={a,b,c}'::text[], 'b')::text
SELECT array_remove('[0:2]={a,b,c}'::text[], 'b')::text, array_replace('[0:2]={a,b,c}'::text[], 'b','x')::text, array_cat('[0:1]={a,b}'::text[], '{c}')::text
SELECT (SELECT array_agg(x) FROM unnest('[0:1]={a,b}'::text[]) x)::text, to_json('[0:1]={a,b}'::text[])::text, trim_array('[0:2]={a,b,c}'::text[],1)::text
SELECT '[0:1]={1,2}'::int[]::text[]::text
SELECT '[2:1]={a}'::text[]
SELECT '[0:1]={a}'::text[]
SELECT '[0:1]={a,b}'::text[]
SELECT id, a::text, array_lower(a, 1), array_dims(a) FROM lbt ORDER BY id
SELECT id, a FROM lbt ORDER BY id
SELECT id, a[0], a[1] FROM lbt ORDER BY id
SELECT id FROM lbt WHERE a = '[0:1]={5,6}' ORDER BY id
UPDATE lbt SET a[0] = 9 WHERE id = 1
UPDATE lbt SET a[5] = 8 WHERE id = 2
SELECT id, a::text FROM lbt ORDER BY id
UPDATE lbt SET a[-1:0] = '{7,7}' WHERE id = 1
SELECT id, a::text FROM lbt ORDER BY id
SELECT %s::int[]::text, array_lower(%s::int[], 1) ||| ['[0:1]={4,5}', '[0:1]={4,5}']
SELECT generate_subscripts('[0:2]={a,b,c}'::text[], 1) AS s
SELECT trim_array('{a,b,c}'::text[], 1)::text, trim_array('{{1,2},{3,4}}'::int[], 1)::text
SELECT trim_array('{a,b,c}'::text[], 4)
SELECT array_cat('{1,2}'::int[], '{3}')::text, array_cat('{3}', '{1,2}'::int[])::text
SELECT id, a[0:0]::text, array_upper(a, 1) FROM lbt ORDER BY id
SELECT '[0:1]={a,b}'::text[] || '[5:6]={c,d}'::text[]
SELECT ('[0:1]={a,b}'::text[])[0:0], ('[3:4]={7,8}'::int[])[3] + 1
SELECT %s::text[] ||| ['[2:3]={x,y}']
